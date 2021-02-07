# { sources ? import ./sources.nix }:

# let
#   pkgs =
#     import sources.nixpkgs { overlays = [ (import sources.nixpkgs-mozilla) ]; };
#   channel = "1.43.1";
#   targets = [ ];
#   chan = pkgs.rustChannelOf { channel = channel; };
# in chan



# nix/rust.nix
{ sources ? import ./sources.nix }:

let
  pkgs =
    import sources.nixpkgs { overlays = [ (import sources.nixpkgs-mozilla) ]; };
  channel = "stable";
  date = "2020-05-25";
  targets = [ ];
  chan = pkgs.latest.rustChannels.stable.rust;
in chan
