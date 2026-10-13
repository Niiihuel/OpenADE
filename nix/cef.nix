{ pkgs ? import <nixpkgs> { } }:
# CEF 154.0.34 for the `cef` feature of agents-window, laid out the way
# cef-rs' export-cef-dir does. shell.nix refers to it: entering the shell
# fetches it once (a 300 MB archive).
#
# Why not NIX_CEF_BINARY=1: cef-rs overrides nixpkgs' cef-binary (147.0.10) to
# 154.0.34, and that package's installPhase runs patchelf on Release/libEGL.so
# and libGLESv2.so, which CEF 154 no longer ships -> the build fails.
#
# The derivation is the feasibility prototype's, name included, so a machine
# that built that one already has this output.
with pkgs;
let
  version = "154.0.34";
  file = "cef_binary_${version}+g14c5a08+chromium-154.0.8037.98_linux64_minimal.tar.bz2";
  # Same list as nixpkgs' cef-binary.
  rpath = lib.makeLibraryPath [
    glib nss nspr atk at-spi2-atk libdrm expat libxkbcommon libgbm gtk3 pango
    cairo alsa-lib dbus at-spi2-core cups libGL udev systemdLibs libxcb libx11
    libxcomposite libxdamage libxext libxfixes libxrandr libxshmfence
  ];
  ccRpath = lib.makeLibraryPath [ stdenv.cc.cc ];
in
stdenv.mkDerivation {
  pname = "cef-binary-spike";
  inherit version;
  src = builtins.fetchurl {
    url = "https://cef-builds.spotifycdn.com/${file}";
    sha256 = "14k4xh3yqdw2i8sxqkar00kzalxklkpvdklrp0ldswj361v2qzp8";
  };
  dontStrip = true;
  dontPatchELF = true;
  installPhase = ''
    runHook preInstall
    sed 's/-O0/-O2/' -i cmake/cef_variables.cmake
    stat -c 'unstripped libcef.so bytes: %s' Release/libcef.so
    # The distribution ships libcef.so with symbols; CEF's README says to strip it.
    strip Release/libcef.so
    stat -c 'stripped libcef.so bytes: %s' Release/libcef.so
    patchelf --set-rpath "${rpath}" --set-interpreter "${stdenv.cc.bintools.dynamicLinker}" Release/chrome-sandbox
    patchelf --add-needed libudev.so --set-rpath "${rpath}" Release/libcef.so
    patchelf --set-rpath "${ccRpath}" Release/libvk_swiftshader.so
    patchelf --set-rpath "${ccRpath}" Release/libvulkan.so.1
    mkdir -p $out
    cp -r Release/. Resources/. $out/
    cp -r CMakeLists.txt cmake include libcef_dll CREDITS.html $out/
    echo '{"type":"minimal","name":"${file}","sha1":""}' > $out/archive.json
    runHook postInstall
  '';
}
