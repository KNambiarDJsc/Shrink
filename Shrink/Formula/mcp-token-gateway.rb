# Formula/mcp-token-gateway.rb
# Update sha256 checksums after each release by running:
#   sha256sum mcp-token-gateway-v<VERSION>-<TARGET>.tar.gz
class McpTokenGateway < Formula
  desc     "Transparent MCP proxy that compacts verbose tool schemas to save context tokens"
  homepage "https://github.com/YOUR_ORG/mcp-token-gateway"
  license  "MIT"
  version  "0.1.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/YOUR_ORG/mcp-token-gateway/releases/download/v#{version}/mcp-token-gateway-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_SHA256_MACOS_ARM64"
    else
      url "https://github.com/YOUR_ORG/mcp-token-gateway/releases/download/v#{version}/mcp-token-gateway-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_SHA256_MACOS_X64"
    end
  end

  on_linux do
    if Hardware::CPU.arm? && Hardware::CPU.is_64_bit?
      url "https://github.com/YOUR_ORG/mcp-token-gateway/releases/download/v#{version}/mcp-token-gateway-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER_SHA256_LINUX_ARM64"
    else
      url "https://github.com/YOUR_ORG/mcp-token-gateway/releases/download/v#{version}/mcp-token-gateway-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER_SHA256_LINUX_X64"
    end
  end

  def install
    bin.install "mcp-token-gateway"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/mcp-token-gateway --version")
  end
end