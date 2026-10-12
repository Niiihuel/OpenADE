{ pkgs ? import <nixpkgs> { } }:
# The normal shell plus CEF, for `--features cef`:
#   nix-shell nix/shell-cef.nix --run "cargo build --profile desktop -p agents-window --features cef"
#   nix-shell nix/shell-cef.nix --run "target/desktop/agents-window"
let
  base = import ../shell.nix { inherit pkgs; };
  cef = import ./cef.nix { inherit pkgs; };
in
base.overrideAttrs (old: {
  # cef-dll-sys links against this directory instead of downloading CEF.
  CEF_PATH = "${cef}";
  # libcef.so is found here at run time, and CEF reads its resources and
  # locales from the directory libcef.so was loaded from.
  LD_LIBRARY_PATH = "${cef}:${old.LD_LIBRARY_PATH}";
})
