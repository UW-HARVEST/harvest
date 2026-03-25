{ pkgs ? import <nixpkgs> { } }:

let
  llvmPkgs = pkgs.llvmPackages_21;
  libclang = llvmPkgs.libclang.lib;
  clangBin = llvmPkgs.clang-unwrapped;
in
pkgs.rustPlatform.buildRustPackage rec {
  pname = "harvest-code";
  version = "0.1.0";
  cargoLock.lockFile = ./Cargo.lock;
  cargoBuildFlags = [ "--bin" "translate" ];
  src = pkgs.lib.cleanSource ./.;
  nativeBuildInputs = with pkgs; [
    rustPlatform.bindgenHook
    makeWrapper
  ];

  buildInputs = [
    libclang
  ];

  # Required by bindgen during the build phase
  LIBCLANG_PATH = "${libclang}/lib";

  postInstall = ''
    wrapProgram $out/bin/translate \
      --set CLANG_PATH "${clangBin}/bin/clang"
  '';
}

