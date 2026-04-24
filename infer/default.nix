{ pkgs ? import <nixpkgs> {} }:

let
    # Pre-built binary sources from the official release notes (shasum -a 256)
    # https://github.com/facebook/infer/releases/tag/v1.2.0
    sources = {
        "x86_64-linux" = {
            url = "https://github.com/facebook/infer/releases/download/v1.2.0/infer-linux-x86_64-v1.2.0.tar.xz";
            sha256 = "21504063fb3a1dbc7919f34dc6e50ca0d35f50b996d91deb7b8bea8243d52d82";
        };
        "aarch64-darwin" = {
            url = "https://github.com/facebook/infer/releases/download/v1.2.0/infer-osx-arm64-v1.2.0.tar.xz";
            sha256 = "dbbb27fade30a2ce26fc65cb6e0c722afaaa0fc3f38cec3f1bd6c35215a60b79";
        };
        "x86_64-darwin" = {
            url = "https://github.com/facebook/infer/releases/download/v1.2.0/infer-osx-x86_64-v1.2.0.tar.xz";
            sha256 = "59f08689f912c5da57cfa630938e3305afa45a732b0e269a02e38fa599f95013";
        };
    };

    source = sources.${pkgs.stdenv.hostPlatform.system};
in pkgs.stdenv.mkDerivation rec {
        pname = "infer";
        version = "1.2.0";

        src = pkgs.fetchurl {
        inherit (source) url sha256;
        };

        nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [
        pkgs.autoPatchelfHook
        ];

        buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [
        pkgs.zlib
        pkgs.zstd
        pkgs.sqlite
        pkgs.libffi
        pkgs.mpfr
        pkgs.gmp
        pkgs.ncurses
        (pkgs.lib.getLib pkgs.libxml2)
        pkgs.stdenv.cc.cc.lib  # libstdc++
        ];

        # These deps are only required by bundled Clang utility binaries
        # (c-index-test, ompdModule.so) that are not used in normal infer operation:
        #   libclang.so.18.1  - bundled Clang's own library, only needed by c-index-test
        #   libxml2.so.2      - only needed by c-index-test
        #   libpython3.8.so.1.0 - only needed by an OpenMP/CUDA GDB debug plugin
        autoPatchelfIgnoreMissingDeps = [
        "libclang.so.18.1"
        "libxml2.so.2"
        "libpython3.8.so.1.0"
        ];

        dontBuild = true;
        dontConfigure = true;

        installPhase = ''
        runHook preInstall

        mkdir -p $out
        cp -r . $out/

        runHook postInstall
        '';

        postInstall = pkgs.lib.optionalString pkgs.stdenv.isLinux (
        let
            # On Nix, system headers live in the store, not /usr/include.
            # Infer's bundled Clang doesn't know about these paths because the
            # Nix gcc wrapper adds them transparently, never as explicit flags.
            #
            # Fix: wrap the bundled clang-18 to inject -isystem paths in driver
            # mode (-### invocations). Clang then translates these to
            # -internal-isystem in the cc1 command, which is what actually runs.
            #
            # We distinguish modes by checking $1:
            #   @file  -> cc1 mode (pass through unchanged)
            #   other  -> driver mode (inject -isystem before other args)
            gccCc = pkgs.gcc.cc;
            gccVersion = gccCc.version;
            gccTarget = pkgs.stdenv.hostPlatform.config;
        in ''
            clangBin="$out/lib/infer/facebook-clang-plugins/clang/install/bin"

            mv "$clangBin/clang-18" "$clangBin/.clang-18-real"

            # clang-18 wrapper: handles all modes infer uses to call clang.
            #
            # Infer's flow:
            #   1. Calls clang-18 @###_file (via sh -c) to get the cc1 command.
            #      The bundled clang ELF reports itself via /proc/self/exe in the
            #      -### output as .clang-18-real. We intercept this output and
            #      rewrite .clang-18-real -> clang-18 so infer calls our wrapper
            #      for the cc1 step too.
            #   2. Calls clang-18 @cc1_file with the args from step 1. We inject
            #      Nix store include paths (-internal-externc-isystem/-internal-isystem)
            #      so the bundled clang finds system headers during analysis.
            #   3. Calls clang-18 -c file.c directly for some files. We inject
            #      -isystem paths so headers are found in driver mode too.
            cat > "$clangBin/clang-18" <<'WRAPPER'
#!/bin/sh
_self=`readlink -f "$0"`
_dir=`dirname "$_self"`
case "$1" in
@*)
_first=`sed -n '1p' "''${1#@}" | tr -d "'\""`
if [ "$_first" = "-###" ]; then
    # Run -### and rewrite .clang-18-real -> clang-18 in output so infer
    # calls this wrapper (not the raw ELF) for the cc1 analysis step.
    _tmp=`mktemp`
    "$_dir/.clang-18-real" "$@" 2>"$_tmp"
    _rc=$?
    sed "s|\"$_dir/.clang-18-real\"|\"$_dir/clang-18\"|g" "$_tmp" >&2
    rm -f "$_tmp"
    exit $_rc
elif [ "$_first" = "-cc1" ]; then
    exec "$_dir/.clang-18-real" "$@" \
    -internal-externc-isystem NIX_GLIBC_INC \
    -internal-isystem NIX_GCC_INC \
    -internal-isystem NIX_GCC_INC_FIXED
else
    exec "$_dir/.clang-18-real" "$@"
fi
;;
*)
exec "$_dir/.clang-18-real" \
    -isystem NIX_GLIBC_INC \
    -isystem NIX_GCC_INC \
    -isystem NIX_GCC_INC_FIXED \
    "$@"
;;
esac
WRAPPER

            sed -i \
            -e 's|NIX_GLIBC_INC|${pkgs.glibc.dev}/include|g' \
            -e 's|NIX_GCC_INC_FIXED|${gccCc}/lib/gcc/${gccTarget}/${gccVersion}/include-fixed|g' \
            -e 's|NIX_GCC_INC|${gccCc}/lib/gcc/${gccTarget}/${gccVersion}/include|g' \
            "$clangBin/clang-18"

            chmod +x "$clangBin/clang-18"
        ''
        );

        # On macOS, clear the quarantine attribute that prevents execution
        postFixup = pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
        find $out/bin -type f -exec xattr -d com.apple.quarantine {} \; 2>/dev/null || true
        '';
}
