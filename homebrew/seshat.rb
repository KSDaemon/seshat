# Homebrew formula for Seshat.
#
# This file is the source of truth maintained inside the seshat repo.
# The release pipeline (`.github/workflows/homebrew-bump.yml`) copies
# this template into the public tap repo (`KSDaemon/homebrew-seshat`)
# on each tag push, substituting the version and per-platform SHA256
# placeholders below.
#
# End users install via:
#   brew tap KSDaemon/seshat
#   brew install seshat
#
# Local install for testing (without the tap):
#   brew install --build-from-source ./homebrew/seshat.rb

class Seshat < Formula
  desc "Operating manual for your codebase, written for AI agents (MCP server)"
  homepage "https://github.com/KSDaemon/seshat"
  version "__VERSION__"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/KSDaemon/seshat/releases/download/v#{version}/seshat-aarch64-apple-darwin.tar.gz"
      sha256 "__SHA256_DARWIN_ARM64__"
    end
    on_intel do
      url "https://github.com/KSDaemon/seshat/releases/download/v#{version}/seshat-x86_64-apple-darwin.tar.gz"
      sha256 "__SHA256_DARWIN_X64__"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/KSDaemon/seshat/releases/download/v#{version}/seshat-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "__SHA256_LINUX_ARM64__"
    end
    on_intel do
      url "https://github.com/KSDaemon/seshat/releases/download/v#{version}/seshat-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "__SHA256_LINUX_X64__"
    end
  end

  def install
    bin.install "seshat"

    # Pre-generated completion scripts ship inside the release tarball
    # under completions/. We hand them to the standard Homebrew helpers
    # so brew installs them into the right path for each shell.
    bash_completion.install "completions/seshat.bash" => "seshat"
    zsh_completion.install  "completions/_seshat"
    fish_completion.install "completions/seshat.fish"
  end

  test do
    # `--version` should print the embedded version. Match loosely so
    # the test survives the "(<git-hash>)" suffix appended at build time.
    assert_match "seshat", shell_output("#{bin}/seshat --version")

    # `completions bash` must produce a parseable bash function.
    output = shell_output("#{bin}/seshat completions bash")
    assert_match "_seshat()", output
  end
end
