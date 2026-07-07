{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = [
    pkgs.rustup
    pkgs.rustPlatform.bindgenHook

    pkgs.claude-code pkgs.pkg-config pkgs.llhttp pkgs.zlib pkgs.pcre2 pkgs.cmake pkgs.python3 pkgs.openssl
  ];
}
