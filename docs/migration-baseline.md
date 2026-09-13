# Migration baseline

Recorded while packaging the standalone repo from the source nix-config checkout.
No license was assigned. The owner chose not to add a LICENSE file.

## Source revision

| Item | Value |
| --- | --- |
| Planned Fleet baseline | nix-config `fbd4b40` (2026-09-12) plus the uncommitted tunnel-test repair |
| Live HEAD at packaging | `f9637f55884d461b3d9799296d78d96527283d89` |
| Committed Fleet sources vs `fbd4b40` | no drift in `lib/fleet.nix`, `modules/fleet`, `scripts/fleet*.sh`, or the Fleet regression Nix files |
| Working-tree Fleet change | repaired `scripts/tests/fleet-tunnel-regression-test.sh` |
| Other live status | untracked `plans/`; no other Fleet paths dirty |

Intervening commits after the plan's last review were homelab work, not Fleet
sources. Unrelated working-tree files were left untouched.

## Tunnel-test repair

HEAD and `fbd4b40` share the unrepaired test:

```
d67e081395680d2cd7223637397521ce079952596dd0ebfb0f92e899f8af55d2  scripts/tests/fleet-tunnel-regression-test.sh  (committed)
```

Working tree at packaging:

```
000a78ca795b678ca1bf93f014a4371169c957e9347030a926bdc367a4d9334f  scripts/tests/fleet-tunnel-regression-test.sh  (repaired)
```

The Nix sandbox has no `/usr/bin/env`. Generated launchctl, TCP-probe, and ps
mocks now use `printf '#!%s\n' "$BASH"`. Cleanup assertions use glob loops
instead of `compgen`. A direct `launchctl print-disabled "gui/$(id -u)"`
preflight stays in so a dead mock fails before Fleet hides probe stderr.

Copying the test from HEAD alone drops those fixes.

## SHA-256 of imported sources

SHA-256 of each Fleet source read for this extraction, from the nix-config
checkout at the live HEAD above, with the repaired working-tree test in place.

```
fb10ae8187fc8f336b243672f8eb0f5acdbbce7474d7bc211618a5d11f71bc87  lib/fleet.nix
bd9758475f17c17c50bc8e639bf2ac0240ae483e55f62a27c4b68c97b8bf6848  modules/fleet/home-manager.nix
4f19509de46fd961c7d945d6df656a938d4b7ad8bae879e0722ae938eed3e5ab  modules/fleet/default-tunnels.nix
0035027039e1db6412009e32464b18dac8b1413d7c9dd327b3bfec7f9284efdd  scripts/fleet.sh
16d28b76867851e7c5b1143c2d1656be8dc4b27f40a4db1812826e01dcc4b1c5  scripts/fleet-tunnels.sh
85f59b5b55f8a7c24834836db2f87e5a8dffdd03dadc06726264da270f317e8c  scripts/fleet-tunnel-runner.sh
d944dda45ddd539895188fcfc51d02b51b09ba658cb39760aceed4a51b4d2fd7  tests/fleet-ssh-regression.nix
dfdfda55e41a649b6355626240f4b81929f14e1c736e9509e537a98ecaef9cbb  tests/fleet-tunnel-regression.nix
e08b877cca6e8b2149abcc8977190d1673695ef462a762986e5e2ba15009ceef  tests/fleet-agent-forwarding-regression.nix
e146baf9dc90a7694e8d893b35d8c619d2a8ac44bfb009e6399c24d3cb65c555  tests/fleet-trust-regression.nix
f411fbdc9c7fa096cd9f1eec3716ecfb8b5c24ab87be408ec3dfae489eda2621  tests/fleet-ghostty-regression.nix
5c9f094d9946ae9d1306dfb492ccffe6a0a718452aac3ab0a98638de24e66927  scripts/tests/fleet-ssh-regression-test.sh
000a78ca795b678ca1bf93f014a4371169c957e9347030a926bdc367a4d9334f  scripts/tests/fleet-tunnel-regression-test.sh
```

Personal inventory, SSH keys, known hosts, and generated contracts stay in
nix-config. They are not copied here.

## Consumer pins used by this flake

Taken from nix-config `flake.lock` (root `nixpkgs` is `nixpkgs_3`, Home Manager
follows that pin):

| Input | Rev | narHash |
| --- | --- | --- |
| nixpkgs (nixos-26.05) | `21a67dc470149f337cecafbe965d8d252a390518` | `sha256-ugpsyk3NM2s87vXfUiIIiibbJ4Pp0JPS5p/3mfs+q+c=` |
| home-manager (release-26.05) | `b1d1b60084970f9d1e2b72662639dab6d039be71` | `sha256-9nt4W0HNNP84B+b37yRs9Zxat1ZR5guJxEZDEjAgex4=` |

That nixpkgs `rustc` is 1.95.0. The Nix package uses that `rustPlatform`. The
newer compiler on Kim must not become the MSRV by accident.

## Still in nix-config until a later gate

A durable flake input, installed-package switch, and host activation are not
part of this packaging work. Legacy Bash scripts remain the installed CLI
until a separate approval.
