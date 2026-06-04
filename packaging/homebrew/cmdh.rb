class Cmdh < Formula
  desc "Decentralized registry and offline search tool for AI Agents"
  homepage "https://cmdhub.org"
  url "https://github.com/Xuepoo/cmdhub-oss/releases/download/v0.1.0/cmdh-macos-x86_64.tar.gz"
  version "0.1.0"
  sha256 "SKIP" # Replace with actual sha256 during release pipeline
  license "MIT"

  on_macos do
    if Hardware::CPU.intel?
      url "https://github.com/Xuepoo/cmdhub-oss/releases/download/v0.1.0/cmdh-macos-x86_64.tar.gz"
    else
      url "https://github.com/Xuepoo/cmdhub-oss/releases/download/v0.1.0/cmdh-macos-aarch64.tar.gz"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/Xuepoo/cmdhub-oss/releases/download/v0.1.0/cmdh-linux-x86_64.tar.gz"
    else
      url "https://github.com/Xuepoo/cmdhub-oss/releases/download/v0.1.0/cmdh-linux-aarch64.tar.gz"
    end
  end

  def install
    bin.install "cmdh"
  end

  test do
    system "#{bin}/cmdh", "--version"
  end
end
