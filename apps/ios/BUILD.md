# Native iOS build

The app is SwiftUI, Foundation and Security, with iOS 17 as its minimum target.
`package.json` and `app.json` are metadata for EAS CLI. There is no Expo SDK,
React Native runtime, JavaScript UI, Metro bundle, CocoaPods dependency, or OTA
JavaScript update mechanism.

The shared Xcode scheme is `OpenADERemote`, inside
`ios/OpenADERemote.xcodeproj`. Source files live in `OpenADERemote/` and XCTest
files in `OpenADERemoteTests/`. Run `python3 scripts/generate_project.py` after
adding or removing Swift files. The project generator requires only Python 3.

## Remote build from Linux

From `apps/ios`, use the wrapper rather than a raw `eas build` command:

```sh
node scripts/eas.mjs inspect simulator
node scripts/eas.mjs build simulator --non-interactive --no-wait
```

The wrapper explicitly scopes `EAS_PROJECT_ROOT` to this directory and enables
`EAS_NO_VCS`. Without this, EAS can archive the enclosing repository even when
invoked inside this directory. `.easignore` permits only build configuration,
Swift sources, tests, the plist and approved asset formats. Before uploading,
the wrapper runs EAS's actual archive inspection, rejects unexpected paths,
symlinks and recognizable private credentials, and prints the file manifest.
The build runs from that inspected temporary snapshot, so edits to the working
copy during upload cannot change which files are sent. Add new source directory
names to `.easignore` when extending the app; only the current directories are
permitted.
Keep secrets out of source files: the small content check is not a secret audit.

The custom simulator workflow runs native `xcodebuild`, XCTest and a simulator
launch/screenshot. Its outputs are a simulator `.app` archive and reports
containing Xcode logs, an `.xcresult` bundle and a screenshot. A simulator build
does not require Apple signing credentials and cannot be installed on an iPhone.
It uses local ad hoc code signing with simulator-only entitlements so Security
framework Keychain calls are exercised with an application identity. These
entitlements are scoped to the simulator SDK and do not replace the provisioning
entitlements required for a registered iPhone.
The runner's Xcode version is recorded in the reports.

## Registered iPhone build

```sh
node scripts/eas.mjs credentials device
node scripts/eas.mjs build device
```

The `device` profile uses internal/ad hoc distribution and EAS-managed signing.
It requires an active Apple Developer Program membership, valid credentials and
registered test devices. EAS can manage ordinary signing setup through its CLI.
If Apple authentication or 2FA is needed, complete it in the interactive CLI;
do not paste passwords or recovery codes into chat or commit them to this repo.
The `device` command does not submit to TestFlight. Automatic submission is
disabled by the wrapper; submissions use a separate explicit command.

## TestFlight

```sh
node scripts/eas.mjs build testflight --non-interactive --no-wait
node scripts/eas.mjs submit testflight --id FINISHED_STORE_BUILD_ID --groups Owner --non-interactive --no-wait
eas submit:status --platform ios --profile testflight --json
```

The `testflight` build profile uses App Store distribution with EAS-managed
signing. The submit command requires the exact ID of a finished store build
from this project. It uploads to App Store Connect/TestFlight; it does not
request a public App Store release, invite testers, or create public testing
groups. The initial app record uses `openADE Remote`, `dev.openade.remote`
and Spanish (Latin America). EAS reuses the authorized account's App Store
Connect API key without putting its contents in this repository.
The wrapper disables EAS's default automatic invitation of every account admin.
The authorized account holder was separately assigned to the private internal
`Owner` group, with public links disabled and no other testers invited.

An upload succeeding is different from Apple finishing processing. Check
`submit:status` or the app's TestFlight page for the actual installable state.
Increment `expo.ios.buildNumber` in `app.json` before a later upload of the same
marketing version. The project generator reads version, build number and bundle
identifier from that file, so subsequent builds use the same metadata source.

Push Notifications were enabled on the Apple App ID on 2026-10-07, after V1
had been accepted and processed. EAS regenerated the Store provisioning profile
while retaining the existing distribution certificate and APNs key. An existing
App Store Connect API key handled authentication; no renewed password or new
credential was required. The installed CLI's capability update payload was
rejected by Apple, so the capability was enabled and read back through Apple's
documented `POST /v1/bundleIdCapabilities` API before EAS continued signing.
Physical push delivery still requires verification on the registered iPhone.
Push events come from the openADE window when push is configured on the PC:
a task needs a permission, asks a question, finished or failed. The PC does not
push while its window has focus, and a notice only says what happened
("openADE" and "Necesita tu permiso"), never the project or the task.

The native resource phase includes the opaque 1024px AppIcon asset and
`PrivacyInfo.xcprivacy`. The privacy manifest declares the app-only UserDefaults
reason `CA92.1` for appearance, the Face ID lock and the last project and agent
chosen for a new task; credentials remain in
Keychain. The app does not contain analytics or tracking SDKs.

## Local Mac build

Open the generated project in a compatible Xcode, select `OpenADERemote`, and
choose an iPhone simulator. For the same verification used by EAS:

```sh
bash scripts/build-simulator.sh
```

The simulator workflow also compiles the Release configuration into separate
DerivedData with signing disabled, so code excluded by `#if DEBUG` is checked
without changing Apple credentials. XCTest and launch verification use the
signed Debug app. Explicit `--ui-fixture=<scene>` launch flags cover the task
list (also empty and offline), a running chat, a queued message, folded tool
work, a permission, a question, a CLI task, a new task, settings, pairing, the
Face ID lock, files, changes, Git diff/commit/comment, source and accessibility
text size. They produce `fixture-*.png` captures using local test data and no
host connection, plus light-appearance captures of the list, chat, permission
and question. The list is in `DebugUIFixtures.scenes` and in the script.
These fixtures are compiled out of Release; their screenshots demonstrate layout,
not an actual PC session. `screenshot.png` captures the first-launch screen.
GitHub Actions runs this script on pushes to `wt/remote-*` branches and uploads
the reports as the `ios-reports` artifact.

Linux can validate configuration and generate the project but cannot compile
SwiftUI for iOS. Only a completed macOS/Xcode build establishes native build
success. Check the actual EAS job status and reports before treating it as
verified.

## Verified on 2026-10-06

- [Final simulator build](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/1b74c385-1b10-41cb-bdaf-a25cc3fcdba5)
  finished with Xcode 26.6: 16 XCTest tests passed, including actual Keychain
  save/read/update/deletion. The app launched in the simulator. An earlier
  unsigned simulator build exposed a Keychain runtime error; simulator ad hoc
  signing and SDK-scoped entitlements corrected it, and the regression test now
  covers this behavior.
- [Registered iPhone build](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/b67e4eb6-3fa9-4640-afba-9fe29dbd75f0)
  finished successfully. EAS registered `dev.openade.remote`, reused an existing
  valid distribution certificate and created an ad hoc profile for one already
  registered iPhone. The IPA metadata confirms `iPhoneOS`, minimum iOS 17.0,
  version 0.1.0 (1), embedded provisioning and signature resources. No XCTest
  plugin is included in the device app.
- [TestFlight store build](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/6de28f3c-4207-4e1c-9abe-2f53434f9c25)
  finished successfully as version 0.1.0 (1), with compiled AppIcon assets,
  the privacy manifest and an App Store provisioning profile. The inspected
  upload contained 35 approved files (about 79 KB compressed).
  [EAS submission](https://expo.dev/accounts/niihuel/projects/openade-remote/submissions/e5d8a550-025e-4f16-8272-ef22f97f81b1)
  was scheduled for [App Store Connect app 6819871735](https://appstoreconnect.apple.com/apps/6819871735/testflight).
  EAS submission finished on 2026-10-06. On 2026-10-07 Apple reports the build
  as `VALID` and `IN_BETA_TESTING`. The private internal `Owner` group contains
  one tester, whose state is `INSTALLED`; there is no public testing link.
  This confirms V1 availability, independently of the later V2 upload.

- [V2 intermediate simulator build](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/459cce69-6ebf-4e12-884e-866a27584551)
  finished as version 0.2.0 (2): 22 XCTest tests passed, including real Keychain
  multi-computer isolation, and the app launched with the QR pairing interface.
  Its immutable inspected archive contained 48 approved files (about 98 KB
  compressed). This checkpoint does not include later attachment/desktop bridge
  additions and is not a V2 TestFlight upload. APNs device registration and
  delivery still require the correctly provisioned app on a physical iPhone.

- [Final V2 simulator build](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/eab91371-0e9a-48e2-9142-1df106fc290f)
  finished as version 0.2.0 (2): Debug and Release compilation both succeeded;
  28 XCTest tests passed with zero failures. The Debug app launched normally
  and produced four explicit fixture captures (chat, files, changes, projects).
  The inspected immutable archive contained 51 approved files, 338,233 bytes
  before compression (about 106 KB compressed), with 28 app Swift sources and
  4 test source files. Its source/configuration matched the working files on
  2026-10-06. Release was compiled without signing for the
  simulator; this is not a signed V2 device/Store build.

## Distribution verified on 2026-10-07

- [V4 intermediate simulator build](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/8b779c72-b4f5-470c-942c-94fea246ca14)
  compiled Debug and Release as **0.4.0 (4)** and passed **37 XCTest tests**,
  including unified diff line numbering, quoted Git paths, old/new review
  comments and durable approval-policy options. The native archive contains
  57 approved files, 463,221 bytes, 33 app Swift sources and five test sources.
  An earlier V4 simulator exposed a SwiftUI type-checking limit; splitting
  the composer panel presentation and DEBUG setup into smaller expressions
  resolved it. The intermediate Store job was canceled without submission.
- [V4 Store build](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/1826e8ac-873c-41e6-b44d-b800da86f329)
  uses the exact simulator source archive, verified by all 57 file hashes.
  It finished and its signed IPA passed provisioning, production APNs, resource
  and Release-only checks. **Do not submit this intermediate build:** visual
  review found absent secondary text in the custom material composer panels.
  It has no TestFlight submission.
- The following source correction uses explicit adaptive grayscale foregrounds
  instead of hierarchical primary/secondary colors, corrects the singular Git
  commit summary and warms up the simulator keyboard before captures. It is
  preserved in `build/remote/prepared-v4-color-fix/source-manifest.json`:
  57 files, 463,794 bytes. EAS rejected its new simulator build before creating
  a job because the Free-plan iOS quota is exhausted until 2026-11-01.
  **This correction has not been compiled or visually verified.** No plan was
  changed, no payment made and no V4 submitted. Resume with a simulator build,
  inspect its actual captures, then build and verify the matching Store source
  before submitting it to the existing Owner group. V3 remains available.

- [V3 final simulator build](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/980869af-a880-4629-b88d-5e8dd97491ed)
  finished as **0.3.0 (3)**. Debug and Release compilation succeeded and all
  **30 XCTest tests passed**. Fourteen fixture captures and the normal connection
  screen were inspected; fixtures include dark/light appearance, accessibility
  text size and each new composer sheet. Camera and on-device speech still need
  physical iPhone verification.
- [V3 Store build](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/8df1e79e-e6bf-43dc-9cf5-4a97884d601f)
  finished as **0.3.0 (3)** at 19:29 UTC. Its immutable source manifest matches
  the final simulator byte-for-byte: 55 approved files, 402,984 bytes, 32 app
  Swift sources and four test sources. The IPA confirms iPhoneOS, iOS 17 minimum,
  `dev.openade.remote`, production APNs in the signature and Store profile,
  `get-task-allow=false`, AppIcon and privacy manifest. XCTest and DEBUG fixture
  markers are absent. Evidence is in
  `build/remote/8df1e79e-e6bf-43dc-9cf5-4a97884d601f/verification.json`.
  This successful build alone does not establish TestFlight availability;
  submission and Apple's processing are separate steps.
- [V3 EAS submission](https://expo.dev/accounts/niihuel/projects/openade-remote/submissions/4d05f5cf-4404-47f6-b9f1-57bdaf4c291f)
  was scheduled for the exact V3 Store build and the existing private `Owner`
  group. No tester was invited and no public link was enabled. Do not submit
  the build again while this job is queued or processing.
  EAS completed the submission. At 20:32 UTC on 2026-10-07, Apple reported
  0.3.0 (3) as `VALID` and `IN_BETA_TESTING`, not expired and associated with
  the private Owner group. Its Apple build ID is
  `6dee429d-00b9-40f6-8ec2-9f711b4a43ee`. V3 is available in TestFlight.

- [V2 Store build](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/49a828eb-85dc-4743-ad46-8a97deb7c6cc)
  finished as version **0.2.0 (2)** at 13:15 UTC. The inspected immutable archive
  contained 51 approved files, 338,813 bytes before compression, with 28 app
  Swift sources and 4 test files. This includes a small text correction after
  the final simulator: an interrupted remote session no longer claims it must
  be started from the desktop. The final Store Release build compiled that
  change successfully; the 28 XCTest results above belong to the preceding
  simulator checkpoint, not a new test run of the text correction.
- The downloaded IPA confirms `iPhoneOS`, minimum iOS 17.0, the expected bundle
  identifier and version, production APNs in both its signed entitlements and
  embedded Store profile, and `get-task-allow=false`. It includes the AppIcon
  and privacy manifest, excludes XCTest, and does not contain the DEBUG fixture
  URL in its binary. Evidence is in
  `build/remote/49a828eb-85dc-4743-ad46-8a97deb7c6cc/verification.json`, alongside
  the immutable source manifest, IPA and build logs.
- [V2 EAS submission](https://expo.dev/accounts/niihuel/projects/openade-remote/submissions/e578d791-d3d5-4b8f-b695-b073e2521e40)
  was scheduled at 13:22 UTC for the existing private `Owner` group and finished
  at 14:43 UTC. At 18:34 UTC Apple confirmed V2 as `VALID`, `IN_BETA_TESTING`,
  not expired, and associated with Owner. V2 is available to update in TestFlight.
  Owner has access to all builds, one tester in state `INSTALLED`, and no public
  link. The tester state alone does not prove that the iPhone has updated to V2.
  No new tester was invited. The optional EAS changelog flag was omitted after
  it was rejected as requiring Enterprise; that rejected request created no job
  and no plan was changed.

The simulator and ad hoc source archives contained 31 approved files (about 32 KB compressed).
They did not contain the desktop repository, account secrets or credentials
files. Downloaded logs, test results, screenshots and the IPA are under the
ignored `build/remote/<build-id>/` directory in this workspace. Download fresh
copies through EAS CLI or the linked build pages when using another checkout.

Physical installation and a live iPhone-to-PC session still require the actual
iPhone and reachable host. The cloud build and simulator checks do not establish
that end-to-end result. No public App Store release was requested.

## Check the existing V2 TestFlight delivery

V1 and V2 have finished processing. The existing V2 submission is `FINISHED`
and Apple has enabled internal testing. Do not submit the same build again.

From `apps/ios`, check the existing delivery without creating another job:

```sh
rtk eas submit:view e578d791-d3d5-4b8f-b695-b073e2521e40 --json
rtk eas submit:status --platform ios --profile testflight --json --non-interactive
```

Apple's processing state and the private Owner group can be checked in
[App Store Connect](https://appstoreconnect.apple.com/apps/6819871735/testflight).
The account's existing installed V1 does not need a new invitation: open
TestFlight, select openADE Remote and update to 0.2.0 (2).

Keep distribution limited to the existing private `Owner` group. No public
TestFlight link, third-party invitations, or public App Store release is part
of this workflow. A physical iPhone is still required to confirm APNs delivery
and the complete iPhone-to-PC connection through the authorized network.

References: [EAS native build support](https://docs.expo.dev/build/),
[custom build steps](https://docs.expo.dev/custom-builds/schema/),
[archive exclusions](https://docs.expo.dev/build-reference/easignore/),
[internal distribution](https://docs.expo.dev/build/internal-distribution/).
