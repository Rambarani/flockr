# Modified by flockr (https://github.com/Rambarani/flockr): changed from herdr v0.9.5 (Apache-2.0). See NOTICE.
{
  lib,
  stdenv,
  rustPlatform,
  callPackage,
  runCommand,
  zig_0_16,
  zstd,
  pkg-config,
  git,
  cctools ? null,
  xcbuild ? null,
}:

let
  manifest = lib.importTOML ../Cargo.toml;
  zigDeps = callPackage ../vendor/libghostty-vt/build.zig.zon.nix {
    name = "flockr-libghostty-vt-zig-cache";
    inherit zstd;
    linkFarm =
      name: entries:
      runCommand name { } ''
        mkdir -p $out
        ${lib.concatMapStringsSep "\n" (entry: ''
          cp -rL ${entry.path} $out/${entry.name}
        '') entries}
      '';
  };
  darwinToolchain = lib.optionals stdenv.hostPlatform.isDarwin [
    cctools
    xcbuild
  ];
in
rustPlatform.buildRustPackage {
  pname = "flockr";
  version = manifest.package.version;

  src = lib.fileset.toSource {
    root = ./..;
    fileset = lib.fileset.intersection (lib.fileset.fromSource (lib.sources.cleanSource ./..)) (
      lib.fileset.unions [
        ../assets
        ../crates
        ../distribution/install.ps1
        ../docs/next/api/flockr-api.schema.json
        ../docs/next/api/flockr-job.schema.json
        ../docs/next/api/flockr-feed.schema.json
        ../examples/jobs/workbox-sync
        ../src
        ../vendor/libghostty-vt
        ../vendor/libghostty-vt.vendor.json
        ../vendor/portable-pty
        ../build.rs
        ../Cargo.lock
        ../Cargo.toml
        ../skills/flockr/SKILL.md
        ../skills/flockr-jobs/SKILL.md
      ]
    );
  };

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  nativeBuildInputs = [
    git
    pkg-config
  ] ++ darwinToolchain;

  env = {
    LIBGHOSTTY_VT_OPTIMIZE = "ReleaseFast";
    LIBGHOSTTY_VT_SIMD = "true";
    LIBGHOSTTY_VT_ZIG_SYSTEM_DIR = zigDeps;
    ZIG = lib.getExe zig_0_16;
  };

  preBuild = ''
    export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-global-cache"
    export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local-cache"
  '';

  # Rust tests are covered by the normal CI workflow. The Nix check is
  # intentionally build-only so it validates packaging inputs without
  # duplicating the full Rust test suite.
  doCheck = false;

  meta = {
    description = "Terminal workspace manager for AI coding agents";
    homepage = "https://github.com/Rambarani/flockr";
    license = lib.licenses.asl20;
    mainProgram = "flockr";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
