# Use dist and Homebrew-managed plugin linking for releases

Tabby's first public release path will use `dist`/`cargo-dist` to publish GitHub Release artifacts, SHA-256 checksums, and a Homebrew formula in the general `yersonargotev/homebrew-tap` tap. The v1 release targets Apple Silicon macOS only and installs the `tabby` binary plus release plugin assets through Homebrew; users then explicitly register the plugin with Herdr using a command like `herdr plugin link "$(brew --prefix tabby)/share/tabby"`.

## Considered Options

- Hand-written GitHub Actions release workflow plus manually maintained Homebrew tap formula.
- `dist`/`cargo-dist` generated GitHub Releases plus Homebrew tap publishing.
- Source-only Homebrew formula that builds Tabby on user machines.
- Herdr marketplace-native install with `herdr plugin install yersonargotev/tabby`.

## Consequences

`dist` keeps release artifacts, checksums, GitHub Releases, and Homebrew publishing in one generated release workflow, reducing bespoke release drift. The first release deliberately avoids Intel macOS, Linux, standalone install scripts, silent Homebrew postinstall registration, and Herdr marketplace listing; Linux support and marketplace-native install can be revisited after the Homebrew path is proven.

Homebrew must install a release-specific Herdr manifest under the package prefix, while the root `herdr-plugin.toml` remains the local development manifest. The release manifest should invoke the binary installed by the same Homebrew package, preferably by a relative path from the linked plugin directory, rather than `target/debug/tabby` or an ambiguous `tabby` from `PATH`.
