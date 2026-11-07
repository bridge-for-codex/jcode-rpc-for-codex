class JcodeMcp < Formula
  desc "Rust RPC client and MCP server for operating jcode workspaces from Codex"
  homepage "https://github.com/bridge-for-codex/jcode-rpc-for-codex"
  version "0.2.1"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.2.1/jcode-mcp-v0.2.1-aarch64-apple-darwin.tar.gz"
      sha256 "28809e7f10c468942093ea69cae393e45b721dfd08f8a9f419a77b97031ba011"
    else
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.2.1/jcode-mcp-v0.2.1-x86_64-apple-darwin.tar.gz"
      sha256 "e507c4f106482c18bf03379d2434c3e9f206939e0642328fc1478ae65dd95b8d"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.2.1/jcode-mcp-v0.2.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "2db8aba267afa7bfded871129e91bfd272a564f0390ccc3854addc0dfe33d4d8"
    else
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.2.1/jcode-mcp-v0.2.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "110e7f3dbafb49367d72af8837bddce06825e970731b576eb1eab78b300e74a7"
    end
  end

  def install
    bin.install "jcode-mcp"
  end

  test do
    assert_match "jcode-mcp", shell_output("#{bin}/jcode-mcp --help")
  end
end
