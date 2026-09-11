#!/usr/bin/env python3
"""Prove a native-input screening bound for the frozen HQ1024V1 coefficient bank.

Offline authoring only. Uses Python integers, not sampled frequency responses or
floating-point convolution, for all coefficient composition and bound algebra.
Requires only Python's standard library. Does not change the input crate.
"""
from __future__ import annotations
import argparse
import ast
from fractions import Fraction
import hashlib
import json
import math
from pathlib import Path
import re
import struct
from typing import Iterator

EXPECTED_SHA256 = '7070c2e9abc255062dd30aaa516c0827d969d238759e14a61c5d1da94a67de9d'
LO, HI = -775, 776
NATIVE_FACTOR = 1024
TAIL_FACTOR = 512

def rust_array(text: str, name: str) -> list:
    match = re.search(r'const\s+' + re.escape(name) + r'\s*:[^=]+=', text)
    if match is None:
        raise ValueError(f'Missing coefficient array: {name}')
    start = text.index('[', match.end())
    end = text.index(';', start)
    value = ast.literal_eval(text[start:end].replace('_', ''))
    if not isinstance(value, list):
        raise ValueError(f'{name} is not an array')
    return value

def common_dyadics(values: list[float]) -> tuple[list[int], int]:
    pairs = [float(v).as_integer_ratio() for v in values]
    exponent = max(d.bit_length() - 1 for _, d in pairs)
    return [n << (exponent - (d.bit_length() - 1)) for n, d in pairs], exponent

def exact_rows(source: Path) -> tuple[Iterator[tuple[int, list[int]]], int, str]:
    raw = source.read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    if digest != EXPECTED_SHA256:
        raise ValueError('Coefficient source differs from the attached frozen baseline: ' + digest)
    text = raw.decode('utf-8')
    half = rust_array(text, 'HQ1024_HALF_DELAY_COEFFICIENTS')
    bank = rust_array(text, 'HQ1024_TAIL_COEFFICIENTS')
    if len(half) != 768 or len(bank) != 512 or any(len(r) != 34 for r in bank):
        raise ValueError('Unexpected coefficient geometry')
    h, he = common_dyadics(half + half[::-1])
    tb, te = common_dyadics([v for r in bank for v in r])
    tail = [tb[i*34:(i+1)*34] for i in range(512)]
    exponent = he + te
    if exponent < 10:
        raise ValueError('Unexpected dyadic denominator')
    unit = 1 << exponent

    def rows() -> Iterator[tuple[int, list[int]]]:
        for p in range(NATIVE_FACTOR + 1):
            row = [0] * (HI - LO + 1)
            if p == 0 or p == NATIVE_FACTOR:
                row[p // NATIVE_FACTOR - LO] = unit
            elif p == TAIL_FACTOR:
                for tap, coefficient in enumerate(h):
                    row[768 - tap - LO] = coefficient << te
            else:
                parity, phase = divmod(p, TAIL_FACTOR)
                for offset, coefficient in zip(range(-16, 18), tail[phase]):
                    if coefficient == 0:
                        continue
                    coarse = parity + offset
                    if coarse % 2 == 0:
                        row[coarse // 2 - LO] += coefficient << he
                    else:
                        top = coarse // 2 + 768 - LO
                        for tap, first in enumerate(h):
                            row[top - tap] += coefficient * first
            yield p, row
    return rows(), exponent, digest

def corrected_residual(p: int, row: list[int], exponent: int) -> tuple[list[int], int, int]:
    """Return Q numerators and affine correction numerators a,b."""
    unit = 1 << exponent
    right_weight = p << (exponent - 10)
    residual = row.copy()
    residual[-LO] -= unit - right_weight
    residual[1-LO] -= right_weight
    m0 = sum(residual)
    m1 = sum(k * v for k, v in zip(range(LO, HI+1), residual))
    a, b = m0-m1, m1
    residual[-LO] -= a
    residual[1-LO] -= b
    if sum(residual) != 0 or sum(k*v for k,v in zip(range(LO,HI+1),residual)) != 0:
        raise ArithmeticError('Moment correction failed')
    prefix = 0
    running_q = 0
    q = []
    for v in residual:
        prefix += v
        running_q += prefix
        q.append(running_q)
    if q[-2:] != [0, 0]:
        raise ArithmeticError('Residual did not terminate')
    return q[:-2], a, b

def upper_float(numerator: int, denominator: int) -> float:
    exact = Fraction(numerator, denominator)
    result = float(exact)
    if Fraction.from_float(result) < exact:
        result = math.nextafter(result, math.inf)
    return math.nextafter(result, math.inf) if numerator else 0.0

def encoded_bound(numerator: int, denominator: int, phase: int) -> dict:
    exact = Fraction(numerator, denominator)
    value = upper_float(numerator, denominator)
    return {'numerator': str(exact.numerator), 'denominator': str(exact.denominator),
            'upper_decimal': value, 'upper_hex': value.hex(),
            'upper_bits': f'0x{struct.unpack("<Q", struct.pack("<d", value))[0]:016x}',
            'maximizing_phase': phase}

def derive(source: Path) -> dict:
    rows, exponent, digest = exact_rows(source)
    maximum_a = maximum_b = gain = 0
    phase_a = phase_b = phase_gain = 0
    checked = 0
    for p, row in rows:
        q, a, b = corrected_residual(p, row, exponent)
        current_a = sum(abs(v) for v in q)
        current_b = abs(a) + abs(b)
        current_gain = sum(abs(v) for v in row)
        if current_a > maximum_a:
            maximum_a, phase_a = current_a, p
        if current_b > maximum_b:
            maximum_b, phase_b = current_b, p
        if current_gain > gain:
            gain, phase_gain = current_gain, p
        checked += 1
    denominator = 1 << exponent
    return {'format': 1, 'coefficient_source_sha256': digest,
            'arithmetic': 'exact integers over a common power-of-two denominator',
            'composite_denominator_exponent': exponent,
            'native_phase_rows_checked_including_right_endpoint': checked,
            'raw_support_inclusive': [LO, HI],
            'second_difference_start_offsets_inclusive': [LO, HI-2],
            'A_raw': encoded_bound(maximum_a, denominator, phase_a),
            'B_raw': encoded_bound(maximum_b, denominator, phase_b),
            'composite_l1': encoded_bound(gain, denominator, phase_gain),
            'identity': 'y[n,p] = (1-t+a[p])*x[n] + (t+b[p])*x[n+1] + sum_j Q[p,j]*Delta2(x)[n+j]',
            'upper': 'P_[a,b] <= (1+B_raw)*S_[a,b] + A_raw*D_[a-775,b+773]',
            'notes': ['Constants enclose real arithmetic on the frozen stored binary64 coefficients.',
                      'Runtime bounds must additionally enclose computed second differences and upper-bound arithmetic.',
                      'These are finite HQ1024 bounds, not ideal-sinc accuracy claims.']}

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--crate', type=Path, required=True, help='Path to crates/tonepoet-true-peak')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = derive(args.crate / 'src' / 'hq1024_coefficients.rs')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True)+'\n', encoding='utf-8')
    print(json.dumps({k: result[k] for k in ['A_raw','B_raw','composite_l1']},indent=2))
    return 0

if __name__ == '__main__':
    raise SystemExit(main())
