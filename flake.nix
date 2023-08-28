{
  description = "Avail Light Client flake";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-utils.follows = "flake-utils";
    };

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-analyzer-src.follows = "";
    };

    # advisory-db = {
    #   url = "github:rustsec/advisory-db";
    #   flake = false;
    # };
  };

  outputs =
    { self
      , nixpkgs
      , crane
      , fenix
      , flake-utils
      # , advisory-db
      , ...
    }: flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };

        inherit (pkgs) lib;

        craneLib = crane.lib.${system};
        src = let
          # Only keeps markdown files (We might use README for docs)
          markdownFilter = path: _type: builtins.match ".*md$" path != null;
          markdownOrCargo = path: type:
            (markdownFilter path type) || (craneLib.filterCargoSources path type);
        in
        lib.cleanSourceWith {
          src = craneLib.path ./.;
          filter = markdownOrCargo;
        };

        # Common arguments can be set here to avoid repeating them later
        commonArgs = {
          inherit src;

          buildInputs = with pkgs; ([
            perl
            clang
          ] ++ lib.optionals stdenv.isDarwin [
            # Additional darwin specific inputs can be set here
            libiconv
          ]);

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        };

        craneLibLLvmTools = craneLib.overrideToolchain
          (fenix.packages.${system}.complete.withComponents [
            "cargo"
            "llvm-tools"
            "rustc"
          ]);

        # Build *just* the cargo dependencies, so we can reuse
        # all of that work (e.g. via cachix) when running in CI
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Build the actual crate itself, reusing the dependency
        # artifacts from above.
        avail-light-client = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          cargoExtraArgs = "--bin avail-light";
          doCheck = false;
        });
      in
      {
        checks = {
          # Build the crate as part of `nix flake check` for convenience
          inherit avail-light-client;

          # Run clippy (and deny all warnings) on the crate source,
          # again, resuing the dependency artifacts from above.
          #
          # Note that this is done as a separate derivation so that
          # we can block the CI if there are issues here, but not
          # prevent downstream consumers from building our crate by itself.
          avail-light-client-clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });

          avail-light-client-doc = craneLib.cargoDoc (commonArgs // {
            inherit cargoArtifacts;
          });

          # Check formatting
          avail-light-client-fmt = craneLib.cargoFmt {
            inherit src;
          };

          # TODO: Audit is commented for now, as we depend on substrate which uses vulnerable dependences
          # # Audit dependencies
          # avail-light-client-audit = craneLib.cargoAudit {
          #   inherit src advisory-db;
          # };

          # Run tests with cargo-nextest
          avail-light-client-nextest = craneLib.cargoNextest (commonArgs // {
            inherit cargoArtifacts;
            partitions = 1;
            partitionType = "count";
          });
        } // lib.optionalAttrs (system == "x86_64-linux") {
          # NB: cargo-tarpaulin only supports x86_64 systems
          # Check code coverage (note: this will not upload coverage anywhere)
          avail-light-client-coverage = craneLib.cargoTarpaulin (commonArgs // {
            inherit cargoArtifacts;
          });
        };

        packages = {
          default = avail-light-client;
          avail-light-client-llvm-coverage = craneLibLLvmTools.cargoLlvmCov (commonArgs // {
            inherit cargoArtifacts;
          });
        };

        apps.default = flake-utils.lib.mkApp {
          drv = avail-light-client;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = builtins.attrValues self.checks.${system};

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        };
      });
}
