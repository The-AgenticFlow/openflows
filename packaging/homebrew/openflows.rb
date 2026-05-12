class Openflows < Formula
  desc "Autonomous AI development team — turns GitHub issues into working PRs"
  homepage "https://openflows.dev"
  url "https://github.com/The-AgenticFlow/AgentFlow.git",
      tag:      "v0.1.0",
      revision: "PLACEHOLDER_SHA"
  license "MIT"

  depends_on "rust" => :build
  depends_on "node"

  def install
    system "cargo", "install", *std_cargo_args(path: "binary")
  end

  test do
    assert_match "OpenFlows", shell_output("#{bin}/agentflow --version")
  end
end
