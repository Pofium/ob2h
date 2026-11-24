---
name: ob2h-setup
description: Check, install or update the ob2h executable, choose the data directory and verify that the ob2h MCP tools of this plugin start. Use when the ob2h tools are missing in a session, when the MCP server ob2h fails to start, when the user asks to install, set up, update or repair ob2h or OB2H, or when the plugin settings ob2h executable or Data directory are unclear.
---

# Set up ob2h for this plugin

This skill prepares ob2h on this machine so that the `ob2h` MCP server of the plugin can
start. It reuses what already works: run each step, check its result and stop at the first
step that fails. Report the command and its complete output.

The plugin has two settings. The user enters them in the plugin configuration of the host;
do not read or edit the configuration files of the host.

| Setting | Meaning |
| --- | --- |
| **ob2h executable** | Command or absolute path of the executable. The default `ob2h` uses the copy on PATH. |
| **Data directory** | Required. Absolute path of the folder with the memory database, memory files, logs and backups. |

## 1. Platform

Prebuilt releases exist for Windows x86-64, Linux x86-64 (glibc and static musl) and macOS
(Apple Silicon and Intel). On another platform, build from source in step 3.

## 2. Find the executable

```bash
ob2h --version
```

If the command is not found, ask the user whether the setting **ob2h executable** names a
path, and run that path with `--version` instead.

- A version is printed: ob2h is installed. Continue with step 4.
- Not found: continue with step 3.

## 3. Install or update ob2h (only with the user's consent)

Show the user what will happen and wait for an explicit yes:

- The release archive is downloaded from `https://github.com/Pofium/ob2h/releases`, its
  SHA-256 is compared with the checksum published next to it, and nothing is installed when
  the check fails.
- One executable is unpacked. No administrator rights are needed.

Then:

1. Read the latest version and the asset URLs from
   `https://api.github.com/repos/Pofium/ob2h/releases/latest`. The assets are named
   `ob2h-<version>-<build>.<ext>`:

   | Machine | Asset |
   | --- | --- |
   | Windows x86-64 | `ob2h-<version>-Windows-x86_64-msvc.zip` |
   | Linux x86-64, static | `ob2h-<version>-Linux-x86_64-musl.tar.gz` |
   | Linux x86-64, glibc | `ob2h-<version>-Linux-x86_64-gnu.tar.gz` |
   | macOS Apple Silicon | `ob2h-<version>-macOS-aarch64.tar.gz` |
   | macOS Intel | `ob2h-<version>-macOS-x86_64.tar.gz` |

2. Download the archive and its `.sha256` file into a new empty directory. Compare the
   SHA-256 of the archive with the digest in the `.sha256` file
   (`sha256sum`, `shasum -a 256` or `Get-FileHash -Algorithm SHA256`). Stop on a mismatch.
3. Unpack the archive. The executable `ob2h` (or `ob2h.exe`) is inside the folder
   `ob2h-<version>/`.
4. Put the executable in a directory that is on PATH (for example `~/.local/bin`), or leave
   it where it is and ask the user to set **ob2h executable** to its full path.
5. On macOS, remove the download attribute when the binary is not notarized:
   `xattr -d com.apple.quarantine <path to ob2h>`.
6. Run `ob2h --version`.

To build from source instead: `git clone https://github.com/Pofium/ob2h.git`, then
`cargo build --release` in the clone. The executable is `target/release/ob2h`.

An existing installation is updated the same way, again only with consent. The user can
also run `ob2h update check` and `ob2h update apply --yes` in their own terminal.

## 4. Data directory

Ask the user for an absolute folder path:

- a new store, for example `~/.ob2h`;
- or the `data` folder of an existing ob2h installation, to keep using that memory.

The user enters the path as the plugin setting **Data directory**. The folder is created
when the server first starts. The folder is not removed when the plugin is uninstalled.

## 5. Verify from the command line

```bash
OB2H_DATA_DIR="<data directory>" ob2h stats
```

On Windows (PowerShell): `$env:OB2H_DATA_DIR = "<data directory>"; ob2h stats`.

Report any error with the complete output. If the user has not entered the settings yet,
ask them to do so now.

## 6. Verify in a session

Ask the user to start a new session, because the MCP server of a plugin starts with the
session. In the new session, call `omnes_stats`. It reports the counters of the store.
If the ob2h tools are still missing, ask the user to check both settings and the MCP server
status in the host settings, and report what they see.

## Decisions that belong to the user

Run these only when the user asks for them, because they change other tools:

- `ob2h agent install ...`, `ob2h plugin install`, `ob2h skill install`: they write MCP
  settings and skills of other agents and IDEs.
- `ob2h doctor --fix`: it repairs those settings and installs git hooks in projects.
- `ob2h update apply`: it replaces the executable.
- `ob2h self uninstall`: it removes the integration and, with `--purge-data`, the store.

Never ask for the PIN of the secrets vault in the conversation.
