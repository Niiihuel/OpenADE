use super::*;

fn version(text: &str) -> semver::Version {
    semver::Version::parse(text).unwrap()
}

fn release_json(tag: &str, assets: &[(&str, &str)]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "tag_name": tag,
        "html_url": format!("https://github.com/Niiihuel/OpenADE/releases/tag/{tag}"),
        "draft": false,
        "prerelease": false,
        "assets": assets
            .iter()
            .map(|(name, url)| serde_json::json!({"name": name, "browser_download_url": url}))
            .collect::<Vec<_>>(),
    }))
    .unwrap()
}

#[test]
fn only_a_newer_stable_release_is_offered_and_only_this_repository_s_signed_archive_is_downloaded() {
    let current = version("0.3.0");
    let name = "openADE-0.4.0-linux-x86_64.tar.gz";
    let download = |file: &str| format!("{DOWNLOADS}v0.4.0/{file}");
    let signed = release_json(
        "v0.4.0",
        &[(name, &download(name)), (&format!("{name}.minisig"), &download(&format!("{name}.minisig")))],
    );
    let release = newer_release(&signed, &current).unwrap().unwrap();
    assert_eq!(release.version, version("0.4.0"));
    assert_eq!(release.page, "https://github.com/Niiihuel/OpenADE/releases/tag/v0.4.0");
    assert_eq!(release.archive, Some((download(name), download(&format!("{name}.minisig")))));

    // Without a signature, or from somewhere else, it is offered but not downloaded.
    let unsigned = release_json("v0.4.0", &[(name, &download(name))]);
    assert_eq!(newer_release(&unsigned, &current).unwrap().unwrap().archive, None);
    let elsewhere = release_json(
        "v0.4.0",
        &[(name, "https://example.com/a.tar.gz"), (&format!("{name}.minisig"), "https://example.com/a.minisig")],
    );
    assert_eq!(newer_release(&elsewhere, &current).unwrap().unwrap().archive, None);

    for tag in ["v0.3.0", "0.2.9", "v0.4.0-rc.1"] {
        assert_eq!(newer_release(&release_json(tag, &[]), &current).unwrap(), None, "{tag}");
    }
    let mut draft: serde_json::Value = serde_json::from_slice(&release_json("v9.0.0", &[])).unwrap();
    draft["prerelease"] = true.into();
    assert_eq!(newer_release(&serde_json::to_vec(&draft).unwrap(), &current).unwrap(), None);
    assert!(newer_release(&release_json("nightly", &[]), &current).is_err());
    assert!(newer_release(b"{", &current).is_err());
}

/// A release archive for `version` holding `marker` in its launcher.
fn archive(dir: &Path, version: &semver::Version, marker: &str) -> PathBuf {
    let path = dir.join(archive_name(version));
    let encoder = flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    let top = archive_name(version).trim_end_matches(".tar.gz").to_owned();
    for (file, body) in [
        ("install.sh", "#!/bin/sh\n"),
        ("openade", marker),
        ("agents-window", "binary"),
        ("cef/libcef.so", "library"),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, format!("{top}/{file}"), body.as_bytes()).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
    path
}

fn sign(keys: &minisign::KeyPair, file: &Path, trusted: &str) -> String {
    minisign::sign(Some(&keys.pk), &keys.sk, File::open(file).unwrap(), Some(trusted), None)
        .unwrap()
        .into_string()
}

#[test]
fn an_archive_is_unpacked_only_when_its_signature_holds_for_its_own_name() {
    let dir = tempfile::tempdir().unwrap();
    let updates = dir.path().join(".openade.update");
    fs::create_dir_all(&updates).unwrap();
    let keys = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let key = keys.pk.to_base64();
    let next = version("0.4.0");
    let file = archive(dir.path(), &next, "new");
    let signature = sign(&keys, &file, &archive_name(&next));

    let staged = stage(&file, &signature, &key, &next, &updates).unwrap();
    assert_eq!(staged, updates.join("0.4.0"));
    assert_eq!(fs::read_to_string(staged.join("openade")).unwrap(), "new");
    assert!(is_archive(&staged));
    assert_eq!(fs::read_dir(&updates).unwrap().count(), 1, "nothing left of the unpacking");

    // Another key, an older archive under this name, or changed bytes.
    let stranger = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let older = sign(&keys, &file, &archive_name(&version("0.3.5")));
    let mut bytes = fs::read(&file).unwrap();
    let tampered = dir.path().join("tampered.tar.gz");
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&tampered, bytes).unwrap();
    fs::remove_dir_all(&staged).unwrap();
    for (archive, signature, key) in [
        (&file, sign(&stranger, &file, &archive_name(&next)), key.as_str()),
        (&file, older, key.as_str()),
        (&tampered, signature.clone(), key.as_str()),
        (&file, "not a signature".to_owned(), key.as_str()),
    ] {
        let error = stage(archive, &signature, key, &next, &updates).unwrap_err();
        assert!(error.contains("firma"), "{error}");
        assert_eq!(fs::read_dir(&updates).unwrap().count(), 0, "nothing unpacked");
    }

    // A signed archive without the files of a release is refused too.
    let empty = dir.path().join("empty.tar.gz");
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        File::create(&empty).unwrap(),
        flate2::Compression::fast(),
    ));
    let mut header = tar::Header::new_gnu();
    header.set_size(1);
    header.set_cksum();
    builder.append_data(&mut header, "readme", &b"x"[..]).unwrap();
    builder.into_inner().unwrap().finish().unwrap();
    let error = stage(&empty, &sign(&keys, &empty, &archive_name(&next)), &key, &next, &updates).unwrap_err();
    assert!(error.contains("archivos esperados"), "{error}");
    assert_eq!(fs::read_dir(&updates).unwrap().count(), 0);
}

#[test]
fn the_release_key_built_in_verifies_what_its_secret_half_signed() {
    let signature = minisign_verify::Signature::decode(
        "untrusted comment: signature from rsign secret key
RUTBGPfzssEDLs7dttOIV6uKRLkylZeul4z2lux9sTOo6zhjqYfKkIdqR1PXWuPEZ0hRsLWlbO6KozRNX7goJNs/Ovar+XD9sQQ=
trusted comment: openADE update key check
AEkNyQQue5+yl7gHDvglX/k3UqJ+tqCSrVtJtHHYQUZLl/sk7Fia72KS+xtdf2B88BFJNoFpK3do/m7v5wYcAQ==",
    )
    .unwrap();
    let key = minisign_verify::PublicKey::from_base64(PUBLIC_KEY).unwrap();
    let mut verifier = key.verify_stream(&signature).unwrap();
    verifier.update(b"openADE update key check\n");
    assert!(verifier.finalize().is_ok());
    let mut changed = key.verify_stream(&signature).unwrap();
    changed.update(b"openADE update key check!\n");
    assert!(changed.finalize().is_err());
}

#[test]
fn a_release_folder_is_an_install_its_staged_version_is_found_and_put_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("openade");
    let make = |path: &Path, marker: &str| {
        fs::create_dir_all(path.join("cef")).unwrap();
        for file in ["install.sh", "agents-window", "cef/libcef.so"] {
            fs::write(path.join(file), "x").unwrap();
        }
        fs::write(path.join("openade"), marker).unwrap();
    };
    assert_eq!(Install::at(&root), None);
    make(&root, "old");
    let install = Install::at(&root).unwrap();
    assert_eq!(install.updates(), dir.path().join(".openade.update"));
    assert_eq!(install.staged(&version("0.3.0")), None);

    for (name, marker) in [("0.3.0", "same"), ("0.4.0", "new"), ("0.3.9", "older"), ("junk", "junk")] {
        make(&install.updates().join(name), marker);
    }
    let (newest, staged) = install.staged(&version("0.3.0")).unwrap();
    assert_eq!((newest, staged.clone()), (version("0.4.0"), install.updates().join("0.4.0")));
    assert_eq!(install.staged(&version("0.4.0")), None);

    // A staged copy that is gone leaves the install as it was.
    let error = apply(&install.updates().join("missing"), &root).unwrap_err();
    assert!(error.contains("versión nueva"), "{error}");
    assert_eq!(fs::read_to_string(root.join("openade")).unwrap(), "old");

    apply(&staged, &root).unwrap();
    assert_eq!(fs::read_to_string(root.join("openade")).unwrap(), "new");
    assert!(!staged.exists() && !dir.path().join(".openade.old").exists());

    assert_eq!(install.last_error(), None);
    fs::write(install.updates().join(LAST_ERROR), "  no se pudo  \n").unwrap();
    assert_eq!(install.last_error().as_deref(), Some("no se pudo"));
}

#[test]
fn the_swap_waits_for_the_old_process_to_exit() {
    let mut child = std::process::Command::new("sleep").arg("0.3").spawn().unwrap();
    let pid = child.id();
    assert!(wait_for_exit(pid, Duration::from_millis(50)).is_err(), "still running");
    // Exited and not reaped yet, as the app is until its parent waits for it.
    assert!(wait_for_exit(pid, Duration::from_secs(5)).is_ok());
    child.wait().unwrap();
}
