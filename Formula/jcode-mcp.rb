class JcodeMcp < Formula
  desc "Rust RPC client and MCP server for operating jcode workspaces from Codex"
  homepage "https://github.com/bridge-for-codex/jcode-rpc-for-codex"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.1.0/jcode-mcp-v0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "aa51e9a9d72eb8872b263d1807c6aa985a11381dc5a905f4134b7e36be271d7e"
    else
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.1.0/jcode-mcp-v0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "5eceefcb3bed8881ae9c5294534dab09df3b3a9e9541c00f3db75255320fd808"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.1.0/jcode-mcp-v0.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0b626c10d2abbbba3d24e7f29e611192047fcc31763295ce453eb6a166e64764"
    else
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.1.0/jcode-mcp-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0ebc5c5b8454e25d19f8849bf01df32ec92439eedbf166d1e215f316fb7ebfa7"
    end
  end

  def install
    bin.install "jcode-mcp"
  end

  test do
    assert_match "jcode-mcp", shell_output("#{bin}/jcode-mcp --help")
  end
end
