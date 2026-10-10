#!/usr/bin/env python3
"""Rebuild reviewed skill manifests from immutable official GitHub commits.

This is a maintainer command, never part of the app's render/install path.
Edit starter-sources.json only after reviewing the new revision, license and
runtime requirements. Every blob under each selected skill directory is kept;
symlinks/submodules, redirects, mutable refs and oversized bundles fail closed.
Downloaded scripts are data and are never executed.

  rtk python3 scripts/update-marketplace-catalog.py --check
  rtk python3 scripts/update-marketplace-catalog.py --cache /tmp/skill-review
"""

import argparse
import concurrent.futures
import hashlib
import json
from pathlib import Path
import re
import tempfile
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "apps/agents-window/assets/marketplace"
PUBLISHERS = {
    "starter-anthropic": "anthropics/skills",
    "starter-vercel": "vercel-labs/agent-skills",
    "starter-supabase": "supabase/agent-skills",
    "starter-sentry": "getsentry/skills",
    "starter-microsoft": "microsoft/skills",
    "starter-cloudflare": "cloudflare/skills",
}
NOTICES = {"README.md", "LICENSE", "LICENSE.md", "LICENSE.txt", "NOTICE", "NOTICE.md", "THIRD_PARTY_NOTICES.md"}
MAX_BYTES = 20 * 1024 * 1024
MAX_FILES = 512


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def fetch(url, limit):
    request = urllib.request.Request(url, headers={"User-Agent": "OpenIDE-catalog-review"})
    with urllib.request.build_opener(NoRedirect).open(request, timeout=30) as response:
        body = response.read(limit + 1)
        if len(body) > limit:
            raise ValueError(f"Response exceeds review limit: {url}")
        return body


def safe_path(value):
    return (isinstance(value, str) and 0 < len(value) <= 512
            and all(part not in {"", ".", ".."} and re.fullmatch(r"[A-Za-z0-9_.-]+", part)
                    for part in value.split("/")))


def repository_tree(cache, repository, revision):
    path = cache / repository / revision / "tree.json"
    if path.exists():
        data = json.loads(path.read_bytes())
    else:
        data = json.loads(fetch(
            f"https://api.github.com/repos/{repository}/git/trees/{revision}?recursive=1",
            32 * 1024 * 1024,
        ))
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(data))
    if data.get("truncated") or not isinstance(data.get("tree"), list):
        raise ValueError(f"Complete Git tree required: {repository}@{revision}")
    return data["tree"]


def reviewed_blob(cache, repository, revision, blob):
    path = blob["path"]
    size = blob.get("size")
    if (not safe_path(path) or blob.get("mode") not in {"100644", "100755"}
            or not isinstance(size, int) or not 0 <= size <= MAX_BYTES):
        raise ValueError(f"Unsupported blob: {repository}/{path}")
    destination = cache / repository / revision / "files" / path
    if destination.exists():
        body = destination.read_bytes()
    else:
        body = fetch(f"https://raw.githubusercontent.com/{repository}/{revision}/{path}", size)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(body)
    git_hash = hashlib.sha1(b"blob " + str(len(body)).encode() + b"\0" + body).hexdigest()
    if len(body) != size or git_hash != blob["sha"]:
        raise ValueError(f"Content differs from the pinned Git tree: {repository}/{path}")
    return body


def generate(cache, selections):
    manifests = []
    trees = {}
    ids = set()
    for selected in selections:
        source, name = selected["id"].split(":", 1)
        repo, revision, directory = (selected[key] for key in ("repository", "revision", "directory"))
        if (PUBLISHERS.get(source) != repo or name != selected["name"]
                or not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", name)
                or not re.fullmatch(r"[a-f0-9]{40}", revision) or not safe_path(directory)
                or selected["id"] in ids):
            raise ValueError(f"Invalid reviewed source: {selected['id']}")
        ids.add(selected["id"])
        key = (repo, revision)
        if key not in trees:
            trees[key] = repository_tree(cache, *key)
        tree = trees[key]
        descendants = [entry for entry in tree if entry["path"].startswith(directory + "/")]
        if any(entry["type"] not in {"tree", "blob"} for entry in descendants):
            raise ValueError(f"Submodule in skill directory: {directory}")
        files = [(blob["path"][len(directory) + 1:], blob)
                 for blob in descendants if blob["type"] == "blob"]
        for notice in selected["upstream_notices"]:
            if notice not in NOTICES:
                raise ValueError(f"Unsupported upstream notice: {notice}")
            blob = next((entry for entry in tree if entry["path"] == notice and entry["type"] == "blob"), None)
            if blob is None:
                raise ValueError(f"Missing upstream license/notice: {repo}/{notice}")
            files.append((f"_upstream/{notice}", blob))
        if (not 0 < len(files) <= MAX_FILES or sum(blob.get("size", MAX_BYTES + 1) for _, blob in files) > MAX_BYTES
                or len({path for path, _ in files}) != len(files)
                or "SKILL.md" not in {path for path, _ in files}):
            raise ValueError(f"Incomplete or oversized skill: {directory}")
        files.sort(key=lambda file: file[0])
        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
            bodies = list(pool.map(lambda file: reviewed_blob(cache, repo, revision, file[1]), files))
        skill = bodies[next(index for index, file in enumerate(files) if file[0] == "SKILL.md")].decode("utf-8")
        frontmatter = skill.split("---", 2)[1] if skill.startswith("---\n") else ""
        if re.search(r"^name:\s*[\"']?" + re.escape(name) + r"[\"']?\s*$", frontmatter, re.MULTILINE) is None:
            raise ValueError(f"Skill name differs from selected metadata: {directory}")
        manifest = {key: value for key, value in selected.items() if key != "upstream_notices"}
        manifest["files"] = [dict(path=path, repository_path=blob["path"], bytes=len(body), sha256=hashlib.sha256(body).hexdigest())
                             for (path, blob), body in zip(files, bodies)]
        manifests.append(manifest)
        print(f"{selected['id']}: {len(files)} files, {sum(len(body) for body in bodies)} bytes", flush=True)
    return (json.dumps(manifests, ensure_ascii=False, indent=2) + "\n").encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, help="Optional review download cache; contents are checked against the Git tree")
    parser.add_argument("--check", action="store_true", help="Verify reproducibility without changing the embedded manifest")
    args = parser.parse_args()
    selections = json.loads((ASSETS / "starter-sources.json").read_bytes())
    with tempfile.TemporaryDirectory(prefix="openide-catalog-review-") as temporary:
        generated = generate(args.cache or Path(temporary), selections)
    output = ASSETS / "starter-catalog.json"
    if args.check:
        if output.read_bytes() != generated:
            raise SystemExit("Embedded catalog differs from the reviewed sources; regenerate and review the diff")
    else:
        with tempfile.NamedTemporaryFile(dir=output.parent, delete=False) as temporary:
            temporary.write(generated)
            pending = Path(temporary.name)
        pending.replace(output)
    print(f"Verified {len(selections)} complete skill bundles ({len(generated)} manifest bytes)")


if __name__ == "__main__":
    main()
