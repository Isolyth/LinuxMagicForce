{
  description = "Linux Magic Trackpad 2 force-click and haptics daemon";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    {
      self,
      nixpkgs,
    }:
    let
      lib = nixpkgs.lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = lib.genAttrs systems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        rec {
          linux-magic-force = pkgs.rustPlatform.buildRustPackage {
            pname = "linux-magic-force";
            version = "0.2.0";

            src = lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;

            postInstall = ''
              install -Dm644 config/force-touch-linux.toml \
                $out/share/linux-magic-force/force-touch-linux.toml
              install -Dm644 config/scroll-haptics.toml \
                $out/share/linux-magic-force/scroll-haptics.toml
              install -Dm644 config/usbc-scroll-haptics.toml \
                $out/share/linux-magic-force/usbc-scroll-haptics.toml
              install -Dm644 config/ridge-haptics.toml \
                $out/share/linux-magic-force/ridge-haptics.toml
              install -d $out/lib/systemd/system
              substitute systemd/force-touchd.service \
                $out/lib/systemd/system/force-touchd.service \
                --replace-fail /usr/local/bin/force-touchd $out/bin/force-touchd \
                --replace-fail /etc/force-touch-linux/config.toml \
                  $out/share/linux-magic-force/force-touch-linux.toml
            '';

            meta = {
              description = "Userspace force-click and haptics daemon for Bluetooth Magic Trackpad 2";
              homepage = "https://github.com/Isolyth/LinuxMagicForce";
              mainProgram = "force-touchd";
              platforms = lib.platforms.linux;
            };
          };

          default = linux-magic-force;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/force-touchd";
          meta.description = "Run force-touchd";
        };
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clippy
              rust-analyzer
              rustc
              rustfmt
              systemd
            ];
          };
        }
      );

      formatter = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        pkgs.nixfmt
      );

      overlays.default = final: _prev: {
        linux-magic-force = self.packages.${final.stdenv.hostPlatform.system}.default;
      };

      nixosModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.services.linuxMagicForce;
          configPath =
            if cfg.configFile != null then
              toString cfg.configFile
            else
              pkgs.writeText "force-touch-linux.toml" cfg.configText;
          extraArgs = lib.optionalString (cfg.extraArgs != [ ]) " ${lib.escapeShellArgs cfg.extraArgs}";
        in
        {
          options.services.linuxMagicForce = {
            enable = lib.mkEnableOption "LinuxMagicForce Magic Trackpad 2 haptic daemon";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
              defaultText = lib.literalExpression "linuxMagicForce.packages.\${pkgs.stdenv.hostPlatform.system}.default";
              description = "Package providing the force-touchd binary.";
            };

            configFile = lib.mkOption {
              type = lib.types.nullOr (lib.types.either lib.types.path lib.types.str);
              default = null;
              example = "/etc/force-touch-linux/config.toml";
              description = "Path to a TOML config file. When set, configText is ignored.";
            };

            configText = lib.mkOption {
              type = lib.types.lines;
              default = builtins.readFile ./config/force-touch-linux.toml;
              defaultText = lib.literalExpression "builtins.readFile ./config/force-touch-linux.toml";
              description = "TOML config content written to the Nix store when configFile is not set.";
            };

            extraArgs = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [ ];
              example = [
                "--no-restore"
              ];
              description = "Extra command-line arguments appended to force-touchd.";
            };
          };

          config = lib.mkIf cfg.enable {
            systemd.services.force-touchd = {
              description = "LinuxMagicForce Magic Trackpad 2 haptic daemon";
              documentation = [ "https://github.com/Isolyth/LinuxMagicForce" ];
              after = [ "bluetooth.target" ];
              wants = [ "bluetooth.target" ];
              wantedBy = [ "multi-user.target" ];

              serviceConfig = {
                Type = "simple";
                ExecStart = "${cfg.package}/bin/force-touchd --config ${configPath}${extraArgs}";
                Restart = "on-failure";
                RestartSec = "2s";
                KillSignal = "SIGTERM";
              };
            };
          };
        };
    };
}
