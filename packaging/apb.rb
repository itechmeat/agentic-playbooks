# Homebrew formula template for apb (Playbooks CLI).
#
# TEMPLATE: after publishing a release, fill in url and sha256 for the actual
# archives (apb-<target>.tar.gz from the release page) and place this in the
# owner's homebrew tap repo (homebrew-tap). Keep the version in sync with the
# release tag.
class Apb < Formula
  desc "Playbooks CLI: YAML-defined agentic playbooks with a web UI"
  homepage "https://github.com/itechmeat/agentic-playbooks"
  version "0.1.0"

  on_macos do
    on_arm do
      url "https://github.com/itechmeat/agentic-playbooks/releases/download/v0.1.0/apb-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_ARM64_SHA256"
    end
    on_intel do
      url "https://github.com/itechmeat/agentic-playbooks/releases/download/v0.1.0/apb-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_X86_64_MACOS_SHA256"
    end
  end

  on_linux do
    # The release only builds x86_64-linux; no binary is provided for ARM Linux.
    on_intel do
      url "https://github.com/itechmeat/agentic-playbooks/releases/download/v0.1.0/apb-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_X86_64_LINUX_SHA256"
    end
  end

  def install
    bin.install "apb"
  end

  test do
    assert_match "apb", shell_output("#{bin}/apb --version")
  end
end
