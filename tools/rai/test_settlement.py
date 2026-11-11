"""settlement(): the validators must decide the same checkpoint state; the
value hash may differ by the round that finalized it"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run_gate_b as g  # noqa: E402

counts = [{'cemented': '5', 'count': '5'}] * 3
def node(value, state, rnd):
    c = {'closed_value': value, 'closed_round': rnd}
    if state:
        c['value'] = state
    return {'epochs': [{'epoch': '0', 'close': c}]}
# same state under parent and child values: agree, rounds differ
s = g.settlement(counts, [node('P', 'S', '0'), node('C', 'S', '1'), node('C', None, '1')], 1)
assert s['values_agree'] and s['settled'] and s['rounds_differ'] == [0], s
# two different states: disagree
s = g.settlement(counts, [node('P', 'S', '0'), node('C', 'T', '1'), node('C', 'T', '1')], 1)
assert not s['values_agree'] and not s['settled'], s
# a caught-up validator with an unknown value: compared by value
s = g.settlement(counts, [node('P', 'S', '0'), node('X', None, '0'), node('P', 'S', '0')], 1)
assert not s['values_agree'], s
s = g.settlement(counts, [node('P', 'S', '0'), node('P', None, '0'), node('P', 'S', '0')], 1)
assert s['values_agree'] and s['rounds_differ'] == [], s
print('ok')
