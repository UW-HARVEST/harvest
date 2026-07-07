let
  # release 24.11, pinned
  nixpkgs = fetchTarball "https://github.com/NixOS/nixpkgs/archive/nixos-26.05.tar.gz";
  pkgs = import nixpkgs { config = {}; overlays = []; };
in pkgs.mkShell {
  buildInputs = [
    pkgs.rustup
    pkgs.rustPlatform.bindgenHook

    pkgs.claude-code pkgs.pkg-config pkgs.llhttp pkgs.zlib pkgs.pcre2 pkgs.cmake pkgs.python3 pkgs.openssl
  ];
}
