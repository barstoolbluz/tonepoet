#!/usr/bin/env python3
"""Exact arithmetic tests for the new input-domain bound (standard library only).

This is not a substitute for tests of the eventual Rust arithmetic/streaming code.
"""
from __future__ import annotations
import argparse
from fractions import Fraction
import json
from pathlib import Path
import random
import unittest
from generate_raw_screen_metadata import (LO, HI, exact_rows, corrected_residual,
                                         common_dyadics, rust_array, upper_float)

CRATE: Path
METADATA: dict

class RawScreenTests(unittest.TestCase):
    def test_all_native_phases_match_independent_cascade_and_identity(self) -> None:
        source = CRATE/'src/hq1024_coefficients.rs'
        text = source.read_text()
        half = rust_array(text,'HQ1024_HALF_DELAY_COEFFICIENTS')
        bank = rust_array(text,'HQ1024_TAIL_COEFFICIENTS')
        h, he = common_dyadics(half+half[::-1])
        t, te = common_dyadics([v for row in bank for v in row])
        rows, exponent, _ = exact_rows(source)
        self.assertEqual(exponent,he+te)
        unit = 1 << exponent
        rng = random.Random(10240910)
        # Exact integer input amplitudes. Linear homogeneity extends the test to
        # arbitrarily scaled dyadic fixtures without introducing test rounding.
        x = [rng.randrange(-65536,65537) for _ in range(HI-LO+1)]
        def sample(k: int) -> int:
            return x[k-LO]
        coarse = {}
        for r in range(-16,19):
            coarse[r] = (sample(r//2) << he) if r%2 == 0 else sum(
                c*sample(r//2+768-tap) for tap,c in enumerate(h))
        d = [x[j]-2*x[j+1]+x[j+2] for j in range(len(x)-2)]
        D = max(map(abs,d)); S = max(abs(sample(0)),abs(sample(1)))
        A = Fraction.from_float(float.fromhex(METADATA['A_raw']['upper_hex']))
        B = Fraction.from_float(float.fromhex(METADATA['B_raw']['upper_hex']))
        upper = (1+B)*S+A*D
        maximum_a = maximum_b = 0
        count = 0
        for p,row in rows:
            y = sum(c*v for c,v in zip(row,x))
            if p in (0,1024):
                other = sample(p//1024)*unit
            elif p == 512:
                other = coarse[1] << te
            else:
                parity,phase = divmod(p,512)
                other = sum(c*coarse[parity+j] for c,j in zip(t[34*phase:34*(phase+1)],range(-16,18)))
            self.assertEqual(y,other, f'cascade mismatch phase {p}')
            q,a,b = corrected_residual(p,row,exponent)
            right = p << (exponent-10)
            reconstructed = (unit-right+a)*sample(0)+(right+b)*sample(1)+sum(c*v for c,v in zip(q,d))
            self.assertEqual(y,reconstructed,f'summation-by-parts mismatch phase {p}')
            self.assertLessEqual(Fraction(abs(y),unit),upper)
            maximum_a = max(maximum_a,sum(map(abs,q)))
            maximum_b = max(maximum_b,abs(a)+abs(b))
            count += 1
        self.assertEqual(count,1025)
        self.assertEqual(upper_float(maximum_a,unit).hex(),METADATA['A_raw']['upper_hex'])
        self.assertEqual(upper_float(maximum_b,unit).hex(),METADATA['B_raw']['upper_hex'])

    def test_constant_and_affine_identity_at_all_phases(self) -> None:
        rows,exponent,_ = exact_rows(CRATE/'src/hq1024_coefficients.rs')
        unit = 1 << exponent
        for p,row in rows:
            q,a,b = corrected_residual(p,row,exponent)
            self.assertEqual(sum(row),unit+a+b)
            self.assertEqual(sum(k*c for k,c in zip(range(LO,HI+1),row)),
                             (p << (exponent-10))+b)

    def test_bin_widening_contains_required_second_difference_starts(self) -> None:
        # Check first/last and short tiles. Local buffer is [-777,T+777].
        for length in [1,2,31,32,255,256,257,4095,4096]:
            d_len = length+1553
            for start in range(0,length,256):
                end = min(start+256,length)
                lo,hi = start+2,end+1550
                self.assertGreaterEqual(lo,0)
                self.assertLess(hi,d_len)
                bin_lo,bin_hi = lo//32,hi//32
                self.assertLessEqual(bin_lo*32,lo)
                self.assertGreaterEqual(min((bin_hi+1)*32,d_len)-1,hi)
                # Absolute required union for native cells start..end-1.
                self.assertEqual(lo-777,start-775)
                self.assertEqual(hi-777,end+773)

    def test_declared_upper_bounds_are_outward(self) -> None:
        for name in ['A_raw','B_raw','composite_l1']:
            bound=METADATA[name]
            exact=Fraction(int(bound['numerator']),int(bound['denominator']))
            self.assertGreaterEqual(Fraction.from_float(float.fromhex(bound['upper_hex'])),exact)

if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--crate',type=Path,required=True)
    parser.add_argument('--metadata',type=Path,default=Path(__file__).resolve().parent/'raw_screen_metadata.json')
    args=parser.parse_args()
    CRATE=args.crate
    METADATA=json.loads(args.metadata.read_text())
    unittest.main(argv=['test_raw_screen.py'],verbosity=2)
