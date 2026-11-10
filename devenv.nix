{ pkgs, inputs, ... }:
let
  inherit (pkgs.lib) optionals optionalString makeSearchPath makeLibraryPath;
  inherit (pkgs.stdenv.hostPlatform) isLinux isDarwin;
  unstable = import inputs.nixpkgs-unstable { system = pkgs.stdenv.system; };
in
{
  env.NGIT_RELAY_TIMEOUT_SECS = 5;
  env.NGIT_RELAY_CONNECT_TIMEOUT_SECS = 5;

  # Tauri on Linux discovers GIO modules (libgiognutls.so for HTTPS)
  # and GSettings schemas only through these vars; nothing sets them
  # implicitly. The plural GSETTINGS_SCHEMAS_PATH is a devenv-local
  # intermediate, not a GLib standard — the actual GLib mechanism is
  # XDG_DATA_DIRS, and the variable is only ever consumed by the
  # enterShell prepend below. An implementation that copies this
  # stanza must preserve the data flow (schema dirs into
  # XDG_DATA_DIRS), not the name.
  env.LD_LIBRARY_PATH = optionalString isLinux
    (makeLibraryPath [ unstable.libayatana-appindicator ]);
  env.GIO_MODULE_DIR = optionalString isLinux
    "${unstable.glib-networking}/lib/gio/modules";
  env.GSETTINGS_SCHEMAS_PATH = optionalString isLinux
    (makeSearchPath "share/gsettings-schemas" [
      unstable.gsettings-desktop-schemas
      unstable.gtk3
    ]);

  languages.javascript.enable = true;
  languages.javascript.bun.enable = true;
  languages.rust.enable = true;
  languages.rust.toolchainFile = ./rust-toolchain.toml;

  packages = with unstable; [
    bats
    cargo-audit
    cargo-deny
    cargo-nextest
    cargo-tauri
    pkg-config
    shellcheck
    # Agent-facing development CLI (verbs encode the correct procedure);
    # see gate.toml and docs/collaboration-workflow.md.
    inputs.gate.packages.${pkgs.stdenv.system}.default
    inputs.ngit.packages.${pkgs.stdenv.hostPlatform.system}.default
  ] ++ optionals isLinux [
    # Fast parallel linker for large debug artifacts. Only wired up when we
    # add .cargo/config.toml with -fuse-ld=mold; falls back to the system
    # linker without it.
    mold
    # Native deps required by Tauri's *-sys crates on Linux
    # (GLib/GTK/WebKit). No Android SDK, no GStreamer/media stack —
    # Wyrd v0.4 has no camera or media path. macOS needs nothing
    # Nix-side: system WKWebView via Xcode, same split as FUSE
    # (stubs for build, system facility at runtime).
    glib
    # Provides GIO's libgiognutls.so TLS backend; without it WebKitGTK
    # fails every HTTPS request with "TLS support is not available".
    # GIO_MODULE_DIR above points GLib at its modules.
    glib-networking
    # Installs org.gtk.* schemas (FileChooser etc.).
    gsettings-desktop-schemas
    gtk3
    webkitgtk_4_1
    libsoup_3
    gdk-pixbuf
    cairo
    pango
    harfbuzz
    atk
    dbus
    librsvg
    # libayatana-appindicator is dlopen'ed by libappindicator-sys at
    # runtime, so it can't be resolved through link-time rpath;
    # LD_LIBRARY_PATH above exposes it.
    libayatana-appindicator
  ] ++ optionals isDarwin [
    # Build-time headers/stubs for fuser's libfuse probes (fuse.pc from
    # nixpkgs, fuse3.pc from our own stubs: nix/macfuse3-stubs.nix).
    macfuse-stubs
    (pkgs.callPackage ./nix/macfuse3-stubs.nix { })
  ];

  # FUSE on darwin is a build/runtime split: the stubs above only cover
  # compiling and linking (headers + fuse.pc/fuse3.pc; devenv wires
  # PKG_CONFIG_PATH to their pkgconfig dirs automatically, so the manual
  # `export PKG_CONFIG_PATH=...` from fuser's README for `nix-env`
  # installs is not needed here). Mounting at runtime still needs the
  # real system macFUSE (kernel extension,
  # /Library/Filesystems/macfuse.fs), which nix cannot provide.
  # Revisit if/when we target FUSE 3 on Linux.

  # GLib resolves GSettings schemas (org.gtk.Settings.FileChooser etc.)
  # at runtime via XDG_DATA_DIRS/<dir>/glib-2.0/schemas — the
  # GSETTINGS_SCHEMAS_PATH above is only consumed by wrapper hooks
  # that unwrapped binaries never see. Prepend the versioned schema
  # dirs so WebKitGTK apps don't abort with "Settings schema ... is
  # not installed".
  enterShell = optionalString isLinux ''
    export XDG_DATA_DIRS="$GSETTINGS_SCHEMAS_PATH:$XDG_DATA_DIRS"
  '';

  git-hooks.hooks = {
    # Mirrors the CI clippy lanes (`rust.yaml` gates-fast: `rust-clippy`
    # plus the mount-free `rust-clippy-nomount` lane): workspace-wide,
    # all targets (unit tests included — bare `cargo clippy` never
    # compiles them), with warnings denied, in both feature
    # configurations. A mount-gated test import once shipped green
    # locally and failed CI exactly here, because the default hook
    # checked neither test targets nor the `--no-default-features`
    # build.
    clippy = {
      enable = true;
      # `language: system` hooks exec the entry directly with no
      # shell, so the two invocations need an explicit `bash -c`
      # wrapper — a bare `&&` would be passed to cargo as literal
      # argv (which is exactly how this read the first time it ran).
      entry = "bash -c \"cargo clippy --workspace --all-targets -- -D warnings && cargo clippy --workspace --all-targets --no-default-features -- -D warnings\"";
      files = "\\.rs$";
      pass_filenames = false;
    };
    rustfmt.enable = true;
    nixpkgs-fmt.enable = true;
    commitizen.enable = true;

    # Dependency-policy gates run on push, not on commit: `cargo audit`
    # fetches the advisory database over the network, and both checks scan
    # the whole lockfile. Policy itself lives in deny.toml; CI enforces the
    # same checks so a push without hooks installed is still caught.
    cargo-audit = {
      enable = true;
      entry = "cargo audit";
      stages = [ "push" ];
      pass_filenames = false;
    };
    cargo-deny = {
      enable = true;
      entry = "cargo deny check";
      stages = [ "push" ];
      pass_filenames = false;
    };

    # Marketing honesty gate: claims on apps/marketing/index.html stay pinned to
    # their backing docs/ sections (apps/marketing/map.json + apps/marketing/map.lock.json).
    # Runs only when the page, the mapping, or the docs change.
    page-check = {
      enable = true;
      name = "page docs mapping";
      entry = "bun run apps/marketing/check.ts";
      files = "^(apps/marketing/|docs/|README\\.md)";
      pass_filenames = false;
    };

    # Documentation link gate: every local Markdown link must resolve
    # (files exist, #anchors match a heading). Archived reviews are
    # frozen evidence and exempt by design. Runs when prose or the
    # checker itself changes; docs-only PRs run no Rust CI, so this is
    # their mechanical check alongside review.
    doc-links = {
      enable = true;
      name = "doc links";
      entry = "bash .ngit/scripts/tests/test-doc-links.sh";
      files = "^(docs/|README\\.md|AGENTS\\.md|CONTRIBUTING\\.md|ROADMAP\\.md|CHANGELOG\\.md|\\.ngit/scripts/tests/test-doc-links\\.sh)$";
      pass_filenames = false;
    };

    # Shell helper tests: the stub suites run when the helpers, their
    # library, or the suites themselves change, so broken ngit ceremony
    # semantics never ship green.
    shell-tests = {
      enable = true;
      name = "shell tests";
      entry = "bash .ngit/scripts/tests/run-all.sh";
      files = "^\\.ngit/scripts/(ngit\\.sh|tests/.*\\.sh)$";
      pass_filenames = false;
    };

    # Shell syntax gate: a stray `done` once shipped and invalidated a
    # 40-minute suite run after all its evidence was collected (bash
    # parses incrementally, so nothing failed until the final step).
    # bash -n parses only its first file argument, so the entry is a
    # wrapper that sweeps every staged shell script (passing them
    # straight to `bash -n` would check exactly one and report green).
    # Pinned by tests/harness/test-bash-syntax.sh.
    bash-syntax = {
      enable = true;
      name = "bash syntax (bash -n)";
      entry = "bash tests/harness/check-bash-syntax.sh";
      files = "\\.sh$";
      pass_filenames = true;
    };
  };

  # Release tooling: `build` wraps `.ngit/scripts/build.sh`,
  # which builds every distribution tarball this machine can produce
  # (native, Rosetta, and remote-builder legs with host capability
  # detection). Input is pinned to the release tag in a detached
  # worktree, never the working copy.
  scripts.build.exec = "${./.ngit/scripts/build.sh} \"$@\"";

  # Desktop development: `wyrd-desktop` runs the Tauri app in dev
  # mode. The Tauri project lives in `crates/wyrd-desktop`
  # (non-default split layout), so the script enters that directory
  # first — the hook paths in `tauri.conf.json` assume it. Extra
  # arguments forward to `cargo tauri dev` (e.g. `-- --drive-dir …`
  # once the binary runs).
  scripts.wyrd-desktop.exec = ''
    cd "$DEVENV_ROOT/crates/wyrd-desktop" && exec cargo tauri dev "$@"
  '';
}
