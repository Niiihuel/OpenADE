#!/usr/bin/env python3
"""Generate the dependency-free Xcode project from the checked-in Swift sources.

No XcodeGen, Ruby gems, Node packages, Expo runtime or CocoaPods are required.
Run this after adding a Swift file and before opening/building the project.
"""
from hashlib import sha256
from pathlib import Path
import json
import re
from xml.sax.saxutils import escape

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT / "ios" / "OpenADERemote.xcodeproj"
app_config = json.loads((ROOT / "app.json").read_text())["expo"]
objects = {}


def ident(name):
    return sha256(name.encode()).hexdigest()[:24].upper()


def add(object_name, isa, **values):
    key = ident(object_name)
    objects[key] = {"isa": isa, **values}
    return key


def scalar(value):
    return value if re.fullmatch(r"[A-Za-z0-9_./]+", value) else json.dumps(value)


def encode(value, level=0):
    indent = "\t" * level
    if isinstance(value, dict):
        lines = ["{"]
        for key, child in value.items():
            lines.append(f"{indent}\t{scalar(key)} = {encode(child, level + 1)};")
        return "\n".join(lines) + f"\n{indent}}}"
    if isinstance(value, list):
        return "(\n" + "".join(f"{indent}\t{encode(item, level + 1)},\n" for item in value) + indent + ")"
    if isinstance(value, int):
        return str(value)
    if value in objects:
        return value + " /* reference */"
    return scalar(value)


app_files = sorted((ROOT / "OpenADERemote").rglob("*.swift"))
test_files = sorted((ROOT / "OpenADERemoteTests").rglob("*.swift"))
if not any(path.name == "OpenADERemoteApp.swift" for path in app_files):
    raise SystemExit("Missing OpenADERemote/OpenADERemoteApp.swift; finish sources before generating the project.")

source_groups = []
phases = {}
for name, files in [("OpenADERemote", app_files), ("OpenADERemoteTests", test_files)]:
    refs, builds, resources = [], [], []
    for file in files:
        relative = file.relative_to(ROOT / name).as_posix()
        ref = add(f"file:{name}/{relative}", "PBXFileReference", lastKnownFileType="sourcecode.swift", path=relative, sourceTree="<group>")
        refs.append(ref)
        builds.append(add(f"build:{name}/{relative}", "PBXBuildFile", fileRef=ref))
    if name == "OpenADERemote":
        refs.append(add("file:Info.plist", "PBXFileReference", lastKnownFileType="text.plist.xml", path="Info.plist", sourceTree="<group>"))
        refs.append(add("file:Simulator.entitlements", "PBXFileReference", lastKnownFileType="text.plist.entitlements", path="Simulator.entitlements", sourceTree="<group>"))
        refs.append(add("file:OpenADERemote.entitlements", "PBXFileReference", lastKnownFileType="text.plist.entitlements", path="OpenADERemote.entitlements", sourceTree="<group>"))
        for resource, file_type in [("Assets.xcassets", "folder.assetcatalog"), ("PrivacyInfo.xcprivacy", "text.xml")]:
            if (ROOT / name / resource).exists():
                ref = add(f"file:{resource}", "PBXFileReference", lastKnownFileType=file_type, path=resource, sourceTree="<group>")
                refs.append(ref)
                resources.append(add(f"build:{resource}", "PBXBuildFile", fileRef=ref))
    source_groups.append(add(f"group:{name}", "PBXGroup", children=refs, name=name, path=f"../{name}", sourceTree="<group>"))
    phases[name] = [
        add(f"sources:{name}", "PBXSourcesBuildPhase", buildActionMask=2147483647, files=builds, runOnlyForDeploymentPostprocessing=0),
        add(f"frameworks:{name}", "PBXFrameworksBuildPhase", buildActionMask=2147483647, files=[], runOnlyForDeploymentPostprocessing=0),
        add(f"resources:{name}", "PBXResourcesBuildPhase", buildActionMask=2147483647, files=resources, runOnlyForDeploymentPostprocessing=0),
    ]

products = []
for name, extension, filetype in [("OpenADERemote", "app", "wrapper.application"), ("OpenADERemoteTests", "xctest", "wrapper.cfbundle")]:
    products.append(add(f"product:{name}", "PBXFileReference", explicitFileType=filetype, includeInIndex=0, path=f"{name}.{extension}", sourceTree="BUILT_PRODUCTS_DIR"))

product_group = add("group:Products", "PBXGroup", children=products, name="Products", sourceTree="<group>")
main_group = add("group:Main", "PBXGroup", children=source_groups + [product_group], sourceTree="<group>")

common = {
    "CLANG_ENABLE_MODULES": "YES", "CLANG_ENABLE_OBJC_ARC": "YES",
    "CLANG_WARN_DOCUMENTATION_COMMENTS": "YES", "CLANG_WARN_UNREACHABLE_CODE": "YES",
    "COPY_PHASE_STRIP": "NO", "ENABLE_USER_SCRIPT_SANDBOXING": "YES",
    "IPHONEOS_DEPLOYMENT_TARGET": "17.0", "SDKROOT": "iphoneos",
    "SWIFT_VERSION": "5.0", "GCC_C_LANGUAGE_STANDARD": "gnu17",
}


def configuration_list(owner, base):
    configs = []
    for config in ["Debug", "Release"]:
        values = dict(base)
        if owner == "Project":
            values.update({
                "DEBUG_INFORMATION_FORMAT": "dwarf" if config == "Debug" else "dwarf-with-dsym",
                "ENABLE_TESTABILITY": "YES" if config == "Debug" else "NO",
                "SWIFT_OPTIMIZATION_LEVEL": "-Onone" if config == "Debug" else "-O",
                "SWIFT_ACTIVE_COMPILATION_CONDITIONS": "DEBUG $(inherited)" if config == "Debug" else "$(inherited)",
                "ONLY_ACTIVE_ARCH": "YES" if config == "Debug" else "NO",
            })
        configs.append(add(f"config:{owner}:{config}", "XCBuildConfiguration", buildSettings=values, name=config))
    return add(f"configs:{owner}", "XCConfigurationList", buildConfigurations=configs, defaultConfigurationIsVisible=0, defaultConfigurationName="Release")


app_settings = {
    "ASSETCATALOG_COMPILER_APPICON_NAME": "AppIcon",
    "CODE_SIGN_STYLE": "Automatic", "CURRENT_PROJECT_VERSION": str(app_config["ios"]["buildNumber"]),
    "CODE_SIGN_ENTITLEMENTS": "../OpenADERemote/OpenADERemote.entitlements",
    "CODE_SIGN_ENTITLEMENTS[sdk=iphonesimulator*]": "../OpenADERemote/Simulator.entitlements",
    "MARKETING_VERSION": app_config["version"], "PRODUCT_BUNDLE_IDENTIFIER": app_config["ios"]["bundleIdentifier"],
    "PRODUCT_NAME": "$(TARGET_NAME)", "INFOPLIST_FILE": "../OpenADERemote/Info.plist",
    "GENERATE_INFOPLIST_FILE": "NO", "TARGETED_DEVICE_FAMILY": "1,2",
    "SUPPORTED_PLATFORMS": "iphoneos iphonesimulator", "SUPPORTS_MACCATALYST": "NO",
    "LD_RUNPATH_SEARCH_PATHS": ["$(inherited)", "@executable_path/Frameworks"],
}
app_target = add("target:App", "PBXNativeTarget", buildConfigurationList=configuration_list("App", app_settings), buildPhases=phases["OpenADERemote"], buildRules=[], dependencies=[], name="OpenADERemote", productName="OpenADERemote", productReference=products[0], productType="com.apple.product-type.application")
proxy = add("proxy:App", "PBXContainerItemProxy", containerPortal=ident("project"), proxyType=1, remoteGlobalIDString=app_target, remoteInfo="OpenADERemote")
dependency = add("dependency:App", "PBXTargetDependency", target=app_target, targetProxy=proxy)
test_settings = {
    "BUNDLE_LOADER": "$(TEST_HOST)", "CODE_SIGN_STYLE": "Automatic",
    "GENERATE_INFOPLIST_FILE": "YES", "PRODUCT_BUNDLE_IDENTIFIER": "dev.openade.remote.tests",
    "PRODUCT_NAME": "$(TARGET_NAME)", "TARGETED_DEVICE_FAMILY": "1,2",
    "TEST_HOST": "$(BUILT_PRODUCTS_DIR)/OpenADERemote.app/$(BUNDLE_EXECUTABLE_FOLDER_PATH)/OpenADERemote",
    "LD_RUNPATH_SEARCH_PATHS": ["$(inherited)", "@executable_path/Frameworks", "@loader_path/Frameworks"],
}
test_target = add("target:Tests", "PBXNativeTarget", buildConfigurationList=configuration_list("Tests", test_settings), buildPhases=phases["OpenADERemoteTests"], buildRules=[], dependencies=[dependency], name="OpenADERemoteTests", productName="OpenADERemoteTests", productReference=products[1], productType="com.apple.product-type.bundle.unit-test")
project_id = add("project", "PBXProject", attributes={"BuildIndependentTargetsInParallel": "YES", "LastUpgradeCheck": "1600", "TargetAttributes": {app_target: {"CreatedOnToolsVersion": "16.0"}, test_target: {"CreatedOnToolsVersion": "16.0", "TestTargetID": app_target}}}, buildConfigurationList=configuration_list("Project", common), compatibilityVersion="Xcode 14.0", developmentRegion="es", hasScannedForEncodings=0, knownRegions=["es", "en", "Base"], mainGroup=main_group, productRefGroup=product_group, projectDirPath="", projectRoot="", targets=[app_target, test_target])

PROJECT.mkdir(parents=True, exist_ok=True)
# EAS's xcode parser uses the conventional section comments to group objects.
sections = []
for isa in sorted({obj['isa'] for obj in objects.values()}):
    sections.append(f"\n/* Begin {isa} section */")
    for key, obj in objects.items():
        if obj['isa'] == isa:
            sections.append(f"\t\t{key} /* {isa} */ = {encode(obj, 2)};")
    sections.append(f"/* End {isa} section */")
document = "// !$*UTF8*$!\n{\n\tarchiveVersion = 1;\n\tclasses = {};\n\tobjectVersion = 56;\n\tobjects = {\n" + '\n'.join(sections) + f"\n\t}};\n\trootObject = {project_id} /* Project object */;\n}}\n"
(PROJECT / "project.pbxproj").write_text(document)


def reference(target, name, extension):
    return f'<BuildableReference BuildableIdentifier="primary" BlueprintIdentifier="{target}" BuildableName="{escape(name)}.{extension}" BlueprintName="{escape(name)}" ReferencedContainer="container:OpenADERemote.xcodeproj"/>'


app_ref = reference(app_target, "OpenADERemote", "app")
test_ref = reference(test_target, "OpenADERemoteTests", "xctest")
scheme = f'''<?xml version="1.0" encoding="UTF-8"?>
<Scheme LastUpgradeVersion="1600" version="1.3">
  <BuildAction parallelizeBuildables="YES" buildImplicitDependencies="YES"><BuildActionEntries>
    <BuildActionEntry buildForTesting="YES" buildForRunning="YES" buildForProfiling="YES" buildForArchiving="YES" buildForAnalyzing="YES">{app_ref}</BuildActionEntry>
    <BuildActionEntry buildForTesting="YES" buildForRunning="NO" buildForProfiling="NO" buildForArchiving="NO" buildForAnalyzing="NO">{test_ref}</BuildActionEntry>
  </BuildActionEntries></BuildAction>
  <TestAction buildConfiguration="Debug" selectedDebuggerIdentifier="Xcode.DebuggerFoundation.Debugger.LLDB" selectedLauncherIdentifier="Xcode.IDEFoundation.Launcher.LLDB" shouldUseLaunchSchemeArgsEnv="YES">
    <Testables><TestableReference skipped="NO">{test_ref}</TestableReference></Testables>
  </TestAction>
  <LaunchAction buildConfiguration="Debug" selectedDebuggerIdentifier="Xcode.DebuggerFoundation.Debugger.LLDB" selectedLauncherIdentifier="Xcode.IDEFoundation.Launcher.LLDB" launchStyle="0" useCustomWorkingDirectory="NO" ignoresPersistentStateOnLaunch="NO" debugDocumentVersioning="YES" debugServiceExtension="internal" allowLocationSimulation="YES">
    <BuildableProductRunnable runnableDebuggingMode="0">{app_ref}</BuildableProductRunnable>
  </LaunchAction>
  <ProfileAction buildConfiguration="Release" shouldUseLaunchSchemeArgsEnv="YES" savedToolIdentifier="" useCustomWorkingDirectory="NO" debugDocumentVersioning="YES"><BuildableProductRunnable runnableDebuggingMode="0">{app_ref}</BuildableProductRunnable></ProfileAction>
  <AnalyzeAction buildConfiguration="Debug"/>
  <ArchiveAction buildConfiguration="Release" revealArchiveInOrganizer="YES"/>
</Scheme>
'''
schemes = PROJECT / "xcshareddata" / "xcschemes"
schemes.mkdir(parents=True, exist_ok=True)
(schemes / "OpenADERemote.xcscheme").write_text(scheme)
print(f"Generated {PROJECT.relative_to(ROOT)}: {len(app_files)} app sources, {len(test_files)} test sources")
