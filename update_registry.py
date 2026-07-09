import json

with open('orchestration/agent/registry.json', 'r') as f:
    data = json.load(f)

for agent in data.get('team', []):
    if 'coder_module' in agent:
        agent['coder_module']['source'] = "registry.coder.com/coder-labs/codex/coder"
        agent['coder_module']['version'] = "5.3.0"
        # Also remove anthropic specific default models if we want to be clean, but let's just update the module
        agent['model_backend'] = "openai/adorsys-coder"

with open('orchestration/agent/registry.json', 'w') as f:
    json.dump(data, f, indent=2)

print("Updated registry.json")
