{ pkgs ? import <nixpkgs> { } }:
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ stdenv.cc pkg-config cmake rustPlatform.bindgenHook ];
  buildInputs = with pkgs; [ wayland libxkbcommon alsa-lib ];
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [ wayland libxkbcommon vulkan-loader libGL alsa-lib stdenv.cc.cc.lib ]);
}
