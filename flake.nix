{
  description = "Rust project with OpenSSL and protoc";
  
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };
  
  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      devShells.${system}.default = pkgs.mkShell {
        buildInputs = with pkgs; [
          rustc
          cargo
          rust-analyzer
          
          # Build dependencies
          openssl
          protobuf
          pkg-config  # Needed for OpenSSL discovery
        ];
        
        # Environment variables for build scripts
        OPENSSL_DIR = "${pkgs.openssl.dev}";
        OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
        PROTOC = "${pkgs.protobuf}/bin/protoc";
        PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
      };
      
      packages.${system}.default = pkgs.rustPlatform.buildRustPackage {
        pname = "my-app";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        
        # Native build dependencies
        nativeBuildInputs = with pkgs; [
          pkg-config
          protobuf
        ];
        
        # Runtime/link dependencies
        buildInputs = with pkgs; [
          openssl
        ];
      };
    };
}
