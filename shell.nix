{ pkgs ? import <nixpkgs> { } }:
let
  # The Chromium the project browser embeds.
  cef = import ./nix/cef.nix { inherit pkgs; };
in
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ stdenv.cc pkg-config cmake rustPlatform.bindgenHook ];
  buildInputs = with pkgs; [ wayland libxkbcommon alsa-lib ];
  # cef-dll-sys links against this directory instead of downloading CEF.
  CEF_PATH = "${cef}";
  # libcef.so is found here at run time, and CEF reads its resources and
  # locales from the directory libcef.so was loaded from. Last on purpose:
  # the directory also holds Chromium's own libvulkan.so.1, which must never
  # come before the system's loader that the application's renderer opens.
  LD_LIBRARY_PATH = "${pkgs.lib.makeLibraryPath (with pkgs; [ wayland libxkbcommon vulkan-loader libGL alsa-lib stdenv.cc.cc.lib ])}:${cef}";
}
