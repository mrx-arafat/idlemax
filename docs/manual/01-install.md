# 1. Install idlemax 🧰

**What you will learn:** how to get the `idlemax` program onto a Mac or a Linux machine, what each step does, and how to check it worked.

idlemax is one program, a single file called `idlemax`.
Installing it means putting that file in a folder your terminal searches for programs (your `PATH`).
Nothing else is installed, and nothing runs in the background until you ask for it in the [next chapter](02-first-run.md).

| You have | Do this |
|---|---|
| macOS (Apple Silicon or Intel) | [Option A: the install script](#option-a-the-install-script-quickest) |
| Linux on x86_64 or arm64 with the glibc of Ubuntu 24.04 or newer | [Option A: the install script](#option-a-the-install-script-quickest) |
| Alpine (musl), Ubuntu 22.04 or another older Linux, or you like building things yourself | [Option B: build from source](#option-b-build-from-source) |
| Windows | Not supported yet: there is no service command, no packaging, and its code paths are neither compiled nor run |

## First, open a terminal

- **macOS:** press `Cmd + Space`, type `Terminal` and press Enter.
- **Linux:** press `Ctrl + Alt + T` on most desktops, or log in over SSH on a server.

Every command below is typed (or pasted) into that window, followed by Enter.

## Option A: the install script (quickest)

The script downloads the latest release for your machine, checks its sha256 checksum, makes sure the new binary runs, and puts it in `~/.local/bin`.
It never needs `sudo`, and any failure leaves your install folder as it was.

1. Look first, change nothing:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/mrx-arafat/idlemax/main/scripts/install.sh | sh -s -- --dry-run
   ```

   It prints the target, the version, the file it would download and where it would put it:

   ```text
   idlemax install (dry run, nothing changed)
     target:  aarch64-apple-darwin
     version: 0.2.0
     ...
   ```

2. Install for real:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/mrx-arafat/idlemax/main/scripts/install.sh | sh
   ```

   ```text
   Downloading idlemax 0.2.0 for aarch64-apple-darwin
   ...
   Installed idlemax 0.2.0
     /Users/you/.local/bin/idlemax

   Next: idlemax setup, then idlemax --help
   ```

3. If the script says the folder **is not on your PATH**, it prints the exact line to add.
   It looks like this:

   ```sh
   export PATH="$HOME/.local/bin:$PATH"
   ```

   Add that line to your shell profile so every new terminal finds `idlemax`:

   - macOS (zsh is the default shell): `echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc`
   - Linux (usually bash): `echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc`

   Then close the terminal and open a new one.

Two environment variables change what the script does:

| Variable | Meaning | Default |
|---|---|---|
| `IDLEMAX_VERSION` | the release to install, for example `0.2.0` | the latest release |
| `IDLEMAX_INSTALL_DIR` | an absolute folder for the binary | `~/.local/bin` |

```sh
curl -fsSL https://raw.githubusercontent.com/mrx-arafat/idlemax/main/scripts/install.sh | IDLEMAX_VERSION=0.2.0 sh
```

> [!NOTE]
> Prebuilt Linux binaries need the glibc of Ubuntu 24.04 or newer.
> On Ubuntu 22.04, another older distribution or musl (Alpine), use Option B.

## Option B: build from source

You build idlemax with Rust's toolchain.
The project is built with Rust 1.95; use 1.95 or newer.
It needs no system OpenSSL or SQLite, because both come built in.

### Step 1: the basic build tools

These give you `git` and a C compiler, which Rust needs to link programs.

- **macOS:**

  ```sh
  xcode-select --install
  ```

  A window pops up; click Install and wait.
  If it says the tools are already installed, you are done.

- **Ubuntu or Debian:**

  ```sh
  sudo apt update
  sudo apt install -y build-essential git curl
  ```

- **Fedora:**

  ```sh
  sudo dnf install -y gcc git curl
  ```

### Step 2: the Rust toolchain

`rustup` installs Rust in your home folder, no `sudo` needed:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Press Enter to accept the default install.
Then load it into the current terminal and check it:

```sh
. "$HOME/.cargo/env"
cargo --version
```

```text
cargo 1.95.0 (...)
```

If your Rust is older, run `rustup update`.

### Step 3: get the code and build it

```sh
git clone https://github.com/mrx-arafat/idlemax
cd idlemax
cargo install --path crates/idlemax
```

What happens:

- `git clone` downloads the source into a folder named `idlemax`.
- `cargo install` compiles a release build and copies the binary to `~/.cargo/bin/idlemax`.
  The first build downloads the dependencies and takes a few minutes.

`rustup` already put `~/.cargo/bin` on your `PATH`, so `idlemax` works in any new terminal.

> [!TIP]
> On a small machine (a 1 or 2 GiB VPS), build with fewer parallel jobs so the build does not run out of memory:
> `CARGO_BUILD_JOBS=2 cargo install --path crates/idlemax`.
> On a server that runs real services, read [Servers](08-servers.md) first: it shows how to build inside a capped unit.

> [!WARNING]
> Use `cargo install`, not a binary from `target/debug` or `target/release`.
> The background service runs the exact file you install it with, and a later rebuild or `cargo clean` would break it.
> `idlemax service install` warns you when it sees a binary inside a cargo `target` folder.

## Check it worked

```sh
idlemax --version
```

```text
0.2.0
```

Then run it with no arguments:

```sh
idlemax
```

Before setup it says so, with two next steps:

```text
idlemax: not set up on this device
help[2]:
  Run `idlemax setup` to create the config and token and install the service
  Run `idlemax doctor` to find what is wrong
```

That is expected: you set it up in the next chapter.

## Updating later

- **Install script:** run it again. It replaces the binary only after the new one runs.
- **From source:** `git pull`, then `cargo install --path crates/idlemax` again.

After an update, restart the background services so they run the new binary: `idlemax service install` restarts a service whose file changed, and `idlemax setup` does the same safely.
Upgrade the coordinator before (or together with) the agents: an older agent with a newer coordinator cannot mark some interruptions as retryable.

## Uninstalling

```sh
idlemax service uninstall     # stop and remove every background service idlemax installed
rm ~/.local/bin/idlemax        # or: cargo uninstall idlemax
rm -rf ~/.idlemax              # only if you also want to delete your settings, token and job history
```

Next: [Your first run](02-first-run.md)
