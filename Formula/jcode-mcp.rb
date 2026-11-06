class JcodeMcp < Formula
  desc "Rust RPC client and MCP server for operating jcode workspaces from Codex"
  homepage "https://github.com/bridge-for-codex/jcode-rpc-for-codex"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.1.0/jcode-mcp-v0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "4e12631ba51d7356cbe360b6379a657f5db59c3e6f4b77942931f8235013899b"
    else
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.1.0/jcode-mcp-v0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "b4c4b93b0b07e546ed96092ded957ee614a8d3b428ed463be9e868066bf33f19"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.1.0/jcode-mcp-v0.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "da09c7ed5176cc81f0708c5ff7272b865848855e78f6807ab3aa202998a9a354"
    else
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.1.0/jcode-mcp-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "2f673c10de2e7223bbbf228413f71bb1903ab939c04bf99effe17131a8da2711"
    end
  end

  def install
    bin.install "jcode-mcp"
  end

  test do
    assert_match "jcode-mcp", shell_output("#{bin}/jcode-mcp --help")
  end
end
