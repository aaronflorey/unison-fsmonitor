{ pkgs ? import <nixpkgs> {} }:

let
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
in
pkgs.rustPlatform.buildRustPackage {
  pname = cargoToml.package.name;
  version = cargoToml.package.version;
  src = ./.;
  cargoLock.lockFile = ./Cargo.lock;

  buildInputs = pkgs.stdenvNoCC.lib.optionals pkgs.stdenvNoCC.isDarwin [
    pkgs.darwin.apple_sdk.frameworks.CoreServices
  ];
}
