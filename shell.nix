let
  sources = import ./nix/sources.nix;
  rust = import ./nix/rust.nix { inherit sources; };
  pkgs = import sources.nixpkgs {};
in
pkgs.mkShell {
  buildInputs = [
    rust
    pkgs.pkgconfig
    pkgs.zlib.dev
    pkgs.zlib.out
    pkgs.zlib
    pkgs.openssl
    pkgs.gtk3
    pkgs.webkitgtk
    pkgs.fontconfig
  ];

  FONTCONFIG_FILE = pkgs.makeFontsConf { fontDirectories = []; };
}
