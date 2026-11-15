# Homebrew formula for Atropos (spec Phase 7). HEAD-only until the first tagged release: then add
# `url` (the release tarball) and `sha256`, and publish this file in a tap
# (`<owner>/homebrew-atropos`, Formula/atropos.rb). Publishing is the maintainer's step.
class Atropos < Formula
  desc "Process manager for developers: from a port or a locked file to the exact process"
  homepage "https://github.com/suchetkumbar/Atropos"
  license any_of: ["MIT", "Apache-2.0"]
  head "https://github.com/suchetkumbar/Atropos.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "build", "--release", "--locked",
           "-p", "atropos-cli", "-p", "atropos-gui", "-p", "atropos-tui"
    bin.install "target/release/atr"
    bin.install "target/release/atropos-tui"
    # `atr` with no arguments starts the GUI next to it (either spelling is found).
    bin.install "target/release/Atropos" => "atropos"
  end

  test do
    assert_match "atr ", shell_output("#{bin}/atr --version")
    # Read-only: nothing listens on this port in the test sandbox.
    assert_match "Nothing is using port 1", shell_output("#{bin}/atr port 1", 1)
  end
end
