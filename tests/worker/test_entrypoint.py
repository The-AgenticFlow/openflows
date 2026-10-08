"""Exercise supervisor behavior without starting a real Docker daemon."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ENTRYPOINT = Path(__file__).resolve().parents[2] / "docker/worker/entrypoint.sh"


class EntrypointTests(unittest.TestCase):
    def run_worker(self, daemon, probe, agent, mapped=True):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = {
                "id": "echo 0",
                "awk": "exit " + ("0" if mapped else "1"),
                "runuser": 'shift 3; exec "$@"',
                "dockerd": daemon,
                "docker": probe,
                "agent": agent,
            }
            for name, body in commands.items():
                path = root / name
                path.write_text("#!/bin/bash\n" + body + "\n")
                path.chmod(0o755)
            # Isolate the socket guard from the developer machine; shorten
            # the readiness deadline while exercising the real supervisor.
            source = ENTRYPOINT.read_text().replace(
                "/var/run/docker.sock", str(root / "docker.sock")
            ).replace("/var/run/docker.pid", str(root / "docker.pid")).replace("i<60", "i<3")
            script = root / "entrypoint.sh"
            script.write_text(source)
            env = dict(os.environ, PATH=str(root) + ":" + os.environ["PATH"])
            result = subprocess.run(
                ["bash", str(script), "agent"], env=env,
                capture_output=True, text=True, timeout=20,
            )
            return result

    def test_agent_exit_status_is_preserved(self):
        result = self.run_worker("exec sleep 100", "exit 0", "echo agent-started; exit 7")
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertIn("agent-started", result.stdout)

    def test_engine_crash_stops_workspace(self):
        result = self.run_worker("sleep 2; exit 1", "exit 0", "exec sleep 100")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Docker daemon stopped", result.stderr)

    def test_readiness_failure_does_not_start_agent(self):
        result = self.run_worker("exec sleep 100", "exit 1", "echo agent-started")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not become ready", result.stderr)
        self.assertNotIn("agent-started", result.stdout)

    def test_rejects_unmapped_root(self):
        result = self.run_worker("exec sleep 100", "exit 0", "echo agent-started", mapped=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("remapped root", result.stderr)
        self.assertNotIn("agent-started", result.stdout)


if __name__ == "__main__":
    unittest.main()
