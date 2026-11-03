"""Check independent chat turns and title requests through the model HTTP API."""
import json
import tempfile
from pathlib import Path

import test_fixtures as fixture_helpers


class ModelRoutesTest(fixture_helpers.FixtureCase):
    def setUp(self):
        with tempfile.TemporaryDirectory() as directory:
            scenario = Path(directory) / 'scenario.json'
            scenario.write_text(json.dumps({'system': {
                'auxiliary': [
                    {'contains': 'Write a short title', 'text': '{"title":"Test journey"}'},
                    {'contains': 'Write a status label', 'text': '{"label":"Working"}'},
                ],
                'turn_contains': ['FORGE task', 'SENTINEL review', 'Continue after gate approval', '/address_review'],
                'routes': [
                    {'contains': ['FORGE task', 'Continue after gate approval'], 'steps': [
                        {'tool': 'execute', 'arguments': {'command': 'forge-step'}},
                        {'requires': {'contains': 'FORGE_OK'}, 'text': 'Forge finished.'}]},
                    {'contains': 'SENTINEL review', 'steps': [
                        {'tool': 'execute', 'arguments': {'command': 'sentinel-step'}},
                        {'requires': {'contains': 'SENTINEL_OK'}, 'text': 'Sentinel finished.'}]},
                    {'contains_all': ['/address_review', 'ROUND_1'], 'steps': [
                        {'tool': 'execute', 'arguments': {'command': 'rework-round-1'}},
                        {'requires': {'contains': 'ROUND_1_OK'}, 'text': 'Round 1 finished.'}]},
                    {'contains_all': ['/address_review', 'ROUND_2'], 'steps': [
                        {'tool': 'execute', 'arguments': {'command': 'rework-round-2'}},
                        {'requires': {'contains': 'ROUND_2_OK'}, 'text': 'Round 2 finished.'}]},
                ]}, 'variables': [
                    {'tool': 'execute', 'arguments': {'command': 'inspect'}},
                    {'tool': 'execute', 'arguments': {'command': 'decide {revision} {round} {head} {chat_plan} {pr_number}'}},
                ], 'truncated': [
                    {'tool': 'execute', 'arguments': {'command': 'inspect'}},
                    {'tool': 'execute', 'arguments': {'command': 'inspect-again'}},
                    {'tool': 'execute', 'arguments': {'command': 'decide {revision} {round} {head}'}},
                ]}))
            self.start('model', scenario)

    request = fixture_helpers.FixtureTest.request

    def turn(self, prompt):
        return {'model': 'system', 'messages': [{'role': 'user', 'content': prompt}],
            'tools': [{'type': 'function', 'function': {'name': 'execute', 'parameters': {
                'type': 'object', 'properties': {'command': {'type': 'string'}},
                'required': ['command']}}}]}

    def complete_tool(self, request, call, output):
        request['messages'].extend([{'role': 'assistant', 'tool_calls': [call]},
            {'role': 'tool', 'tool_call_id': call['id'], 'content': output}])
        code, body, _ = self.request('/v1/chat/completions', request)
        self.assertEqual(code, 200, body)
        return json.loads(body)['choices'][0]['message']['content']

    def test_roles_and_followup_turns_do_not_consume_each_others_scripts(self):
        forge, sentinel = self.turn('FORGE task T-1'), self.turn('SENTINEL review T-1')
        calls = []
        for request, command in [(forge, 'forge-step'), (sentinel, 'sentinel-step')]:
            code, body, _ = self.request('/v1/chat/completions', request)
            self.assertEqual(code, 200, body)
            call = json.loads(body)['choices'][0]['message']['tool_calls'][0]
            self.assertEqual(json.loads(call['function']['arguments'])['command'], command)
            calls.append(call)
        self.assertNotEqual(calls[0]['id'], calls[1]['id'])
        title = {'model': 'system', 'messages': [{'role': 'system', 'content': 'Write a short title'}]}
        code, body, _ = self.request('/v1/chat/completions', title)
        self.assertEqual(code, 200, body)
        title['messages'][0]['content'] = 'Write a status label'
        code, body, _ = self.request('/v1/chat/completions', title)
        self.assertEqual(code, 200, body)
        self.assertEqual(self.complete_tool(sentinel, calls[1], 'SENTINEL_OK'), 'Sentinel finished.')
        self.assertEqual(self.complete_tool(forge, calls[0], 'FORGE_OK'), 'Forge finished.')
        forge['messages'].append({'role': 'user', 'content': 'Continue after gate approval'})
        code, body, _ = self.request('/v1/chat/completions', forge)
        self.assertEqual(code, 200, body)
        next_call = json.loads(body)['choices'][0]['message']['tool_calls'][0]
        self.assertNotEqual(next_call['id'], calls[0]['id'])
        self.assertEqual(self.complete_tool(forge, next_call, 'FORGE_OK'), 'Forge finished.')

    def test_unrecognized_conversations_fail_closed(self):
        self.assertEqual(self.request('/v1/chat/completions', self.turn('Unrelated task'))[0], 400)

    def test_hook_context_does_not_restart_the_current_controller_turn(self):
        request = self.turn('FORGE task T-1')
        code, body, _ = self.request('/v1/chat/completions', request)
        self.assertEqual(code, 200, body)
        call = json.loads(body)['choices'][0]['message']['tool_calls'][0]
        request['messages'].extend([{'role': 'assistant', 'tool_calls': [call]},
            {'role': 'tool', 'tool_call_id': call['id'], 'content': 'FORGE_OK'},
            {'role': 'user', 'content': 'Planning: read source before implementation.'}])
        code, body, _ = self.request('/v1/chat/completions', request)
        self.assertEqual(code, 200, body)
        self.assertEqual(json.loads(body)['choices'][0]['message']['content'], 'Forge finished.')

    def test_review_arguments_use_the_actual_tool_result_and_chat_plan_path(self):
        plan = '/home/coder/.coder/plans/PLAN-12345678-1234-1234-1234-123456789012.md'
        request = self.turn('Use this chat plan: ' + plan)
        request['model'] = 'variables'
        code, body, _ = self.request('/v1/chat/completions', request)
        self.assertEqual(code, 200, body)
        call = json.loads(body)['choices'][0]['message']['tool_calls'][0]
        state = {'phase': 'testing', 'revision': 4, 'review_round': 7, 'head': 'a' * 40, 'pr_number': 2}
        request['messages'].extend([{'role': 'assistant', 'tool_calls': [call]},
            {'role': 'tool', 'tool_call_id': call['id'], 'content': json.dumps({'output': json.dumps(state)})}])
        code, body, _ = self.request('/v1/chat/completions', request)
        self.assertEqual(code, 200, body)
        arguments = json.loads(json.loads(body)['choices'][0]['message']['tool_calls'][0]['function']['arguments'])
        self.assertEqual(arguments['command'], 'decide 4 7 ' + 'a' * 40 + ' ' + plan + ' 2')

    def test_truncated_current_status_cannot_reuse_a_previous_review_round(self):
        request = self.turn('Inspect the current candidate')
        request['model'] = 'truncated'
        code, body, _ = self.request('/v1/chat/completions', request)
        self.assertEqual(code, 200, body)
        call = json.loads(body)['choices'][0]['message']['tool_calls'][0]
        old = json.dumps({'phase': 'testing', 'revision': 1, 'review_round': 2, 'head': 'a' * 40})
        request['messages'].extend([{'role': 'assistant', 'tool_calls': [call]},
            {'role': 'tool', 'tool_call_id': call['id'], 'content': old}])
        code, body, _ = self.request('/v1/chat/completions', request)
        self.assertEqual(code, 200, body)
        call = json.loads(body)['choices'][0]['message']['tool_calls'][0]
        request['messages'].extend([{'role': 'assistant', 'tool_calls': [call]},
            {'role': 'tool', 'tool_call_id': call['id'],
             'content': '{"phase":"submit","review_round":4,"head": ... [truncated]'}])
        code, body, _ = self.request('/v1/chat/completions', request)
        self.assertEqual(code, 400, body)

    def test_contains_all_disambiguates_multiple_rework_rounds(self):
        r1 = self.turn('/address_review\nreason: ROUND_1')
        code, body, _ = self.request('/v1/chat/completions', r1)
        self.assertEqual(code, 200, body)
        call1 = json.loads(body)['choices'][0]['message']['tool_calls'][0]
        self.assertEqual(json.loads(call1['function']['arguments'])['command'], 'rework-round-1')
        self.assertEqual(self.complete_tool(r1, call1, 'ROUND_1_OK'), 'Round 1 finished.')
        r1['messages'].append({'role': 'user', 'content': '/address_review\nreason: ROUND_2'})
        code, body, _ = self.request('/v1/chat/completions', r1)
        self.assertEqual(code, 200, body)
        call2 = json.loads(body)['choices'][0]['message']['tool_calls'][0]
        self.assertEqual(json.loads(call2['function']['arguments'])['command'], 'rework-round-2')
        self.assertEqual(self.complete_tool(r1, call2, 'ROUND_2_OK'), 'Round 2 finished.')
