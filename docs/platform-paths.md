# Platform filesystem contract

The Rust desktop host resolves LSDJ's filesystem roots once during Tauri setup,
before it starts the deck sidecars, generation server, watchers, or installers.
Python services receive the resolved paths through environment variables and do
not derive platform locations from a home directory.

| Ownership | macOS | Windows | Linux |
| --- | --- | --- | --- |
| Configuration | `~/Library/Application Support/works.protocol.lsdj` | `%LOCALAPPDATA%\LSDJ\config` | `$XDG_CONFIG_HOME/lsdj` |
| Durable user data | `~/Documents/LSDJ` | `%LOCALAPPDATA%\LSDJ\data` | `$XDG_DATA_HOME/lsdj` |
| Disposable cache | `~/Library/Caches/works.protocol.lsdj` | `%LOCALAPPDATA%\LSDJ\cache` | `$XDG_CACHE_HOME/lsdj` |
| Downloaded assets | `~/Library/Application Support/LSDJ` | `%LOCALAPPDATA%\LSDJ\assets` | `$XDG_DATA_HOME/lsdj/assets` |
| Install staging | `~/Library/Application Support/LSDJ/.staging` | `%LOCALAPPDATA%\LSDJ\staging` | `$XDG_DATA_HOME/lsdj/staging` |

On Linux, absent or invalid XDG variables use the standard fallbacks
`~/.config`, `~/.local/share`, and `~/.cache`. On Windows, every root is
non-roaming and the short `LSDJ` directory deliberately avoids consuming path
budget when long-path support is disabled. Staging and downloaded assets always
share a filesystem so a validated install can be promoted atomically.

The host exports `LSDJ_CONFIG_HOME`, `LSDJ_DATA_HOME`, `LSDJ_CACHE_HOME`,
`LSDJ_ASSETS_HOME`, and `LSDJ_STAGING_HOME`. It also supplies the current
compatibility variables `MAGENTA_HOME`, `SA3_MLX_HOME`, and `SA3_LORAS_HOME`;
explicit developer/user values for those three are captured into the contract
at startup. Paths are passed as native process-environment values and executable
arguments, not interpolated into shell command strings.

## macOS compatibility and migration

The contract preserves the current visible locations: generated songs and
samples remain in Documents, model assets remain in Application Support, and
settings/MCP credentials remain under the bundle identifier. Startup retains
the existing one-time migrations from `LSDJai` and from
`~/Documents/Magenta/magenta-rt-v2`.

Each migration is an atomic same-filesystem rename attempted only when the
destination does not exist. A restart sees the destination and does nothing. If
preparing or renaming fails, the process contract points the relevant backend at
the old directory for that run, so a migration failure cannot hide an installed
model or adapter.

## Virtual environments

Virtual-environment interpreters are resolved centrally as `bin/python` on
macOS/Linux and `Scripts/python.exe` on Windows. The interpreter and each
argument remain separate process arguments, including when a profile path
contains spaces or non-ASCII characters.
