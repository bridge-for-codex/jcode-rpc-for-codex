class JcodeMcp < Formula
  desc "Rust RPC client and MCP server for operating jcode workspaces from Codex"
  homepage "https://github.com/bridge-for-codex/jcode-rpc-for-codex"
  version "0.2.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.2.0/jcode-mcp-v0.2.0-aarch64-apple-darwin.tar.gz"
      sha256 "dcd8d53aa5229db7b008442a9b4246c26de069a30a72c5cb61d92adeff821a8a"
    else
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.2.0/jcode-mcp-v0.2.0-x86_64-apple-darwin.tar.gz"
      sha256 "054c22debfa19ee2f58e6d20be9759afb21a32837e101e931e0fe84762c5bd00"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.2.0/jcode-mcp-v0.2.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "ca0470f52f5fcdc0922ac47b8e37353c6c1d2e062be5c399d8f044ef80a6125f"
    else
      url "https://github.com/bridge-for-codex/jcode-rpc-for-codex/releases/download/v0.2.0/jcode-mcp-v0.2.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5bd50918db93c0a713a5befa24b9ba7af80a9a243291e5be35851b60e8fffd37"
    end
  end

  def install
    bin.install "jcode-mcp"
  end

  test do
    assert_match "jcode-mcp", shell_output("#{bin}/jcode-mcp --help")
  end
end
