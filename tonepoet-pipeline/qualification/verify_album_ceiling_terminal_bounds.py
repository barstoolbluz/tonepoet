#!/usr/bin/env python3
"""Offline audit of album NormalizePeak terminal-error constants.

Developer qualification only. This file is not referenced by Cargo, build.rs,
flake.nix, or runtime code. Coefficients are transcribed from the pinned SoX-ng
14.8.0.1 source revision 324b8cf873fd7836e8848bd87f7a90d8faa6f849.
"""
from __future__ import annotations

import argparse
import json
import math
import pathlib
import re


OVERSAMPLE = 64
HALF_DELAY_TAPS = 384
LATER_TAPS = (49, 25, 17, 13, 9)
HEADROOM_RECONSTRUCTION_LINF_UPPER = 4.09

# Diagnostic upward-rounded target-LSB bounds for the *interior LTI* terminal
# perturbation. These combine the selected SoX shaping transfer with
# Tonepoet's uncalibrated Headroom64x reconstruction, but they deliberately are
# NOT production authority: the product uses RepeatEndpoints, whose finite edge
# operator can repeat a worst stored error outside the stream. Production uses
# stored-sample support * HEADROOM_RECONSTRUCTION_LINF_UPPER until an edge-aware
# combined proof exists.
EXPECTED_INTERIOR_RECONSTRUCTED = {
    "lipshitz-44100": 41.402224,
    "f-weighted-46000": 93.070747,
    "modified-e-weighted-46000": 23.193027,
    "improved-e-weighted-46000": 161.644890,
    "shibata-48000": 101.321908,
    "shibata-44100": 129.439766,
    "shibata-37800": 61.646534,
    "shibata-32000": 34.863781,
    "shibata-22050": 9.323517,
    "shibata-16000": 11.839719,
    "shibata-11025": 11.950629,
    "shibata-8000": 12.723352,
    "low-shibata-48000": 44.793089,
    "low-shibata-44100": 53.131788,
    "high-shibata-44100": 239.334865,
    "gesemann-44100": 46.558393,
    "gesemann-48000": 42.477318,
}

FIR = {
    "lipshitz-44100": [2.033, -2.165, 1.959, -1.590, .6149],
    "f-weighted-46000": [2.412, -3.370, 3.937, -4.174, 3.353, -2.205, 1.281, -.569, .0847],
    "modified-e-weighted-46000": [1.662, -1.263, .4827, -.2913, .1268, -.1124, .03252, -.01265, -.03524],
    "improved-e-weighted-46000": [2.847, -4.685, 6.214, -7.184, 6.639, -5.032, 3.263, -1.632, .4191],
    "shibata-48000": [2.8720729351043701172, -5.0413231849670410156, 6.2442994117736816406, -5.8483986854553222656, 3.7067542076110839844, -1.0495119094848632812, -1.1830236911773681641, 2.1126792430877685547, -1.9094531536102294922, .99913084506988525391, -.17090806365013122559, -.32615602016448974609, .39127644896507263184, -.26876461505889892578, .097676105797290802002, -.023473845794796943665],
    "shibata-44100": [2.6773197650909423828, -4.8308925628662109375, 6.570110321044921875, -7.4572014808654785156, 6.7263274192810058594, -4.8481650352478027344, 2.0412089824676513672, .7006359100341796875, -2.9537565708160400391, 4.0800385475158691406, -4.1845216751098632812, 3.3311812877655029297, -2.1179926395416259766, .879302978515625, -.031759146600961685181, -.42382788658142089844, .47882103919982910156, -.35490813851356506348, .17496839165687561035, -.060908168554306030273],
    "shibata-37800": [1.6335992813110351562, -2.2615492343902587891, 2.4077029228210449219, -2.6341717243194580078, 2.1440362930297851562, -1.8153258562088012695, 1.0816224813461303711, -.70302653312683105469, .15991993248462677002, .041549518704414367676, -.29416576027870178223, .2518316805362701416, -.27766478061676025391, .15785403549671173096, -.10165894031524658203, .016833892092108726501],
    "shibata-32000": [.82118552923202515, -1.0063692331314087, .62341964244842529, -1.0447187423706055, .64532512426376343, -.87615132331848145, .52219754457473755, -.67434263229370117, .44954317808151245, -.52557498216629028, .34567299485206604, -.39618203043937683, .26791760325431824, -.28936097025871277, .1883765310049057, -.19097308814525604, .10431359708309174, -.10633844882249832, .046832218766212463, -.039653312414884567],
    "shibata-22050": [.056581053882837296, -.56956905126571655, -.40727734565734863, -.33870288729667664, -.29810553789138794, -.19039161503314972, -.16510021686553955, -.13468159735202789, -.096633769571781158, -.081049129366874695, -.064953058958053589, -.054459091275930405, -.043378707021474838, -.03660014271736145, -.026256965473294258, -.018786206841468811, -.013387725688517094, -.0090983230620622635, -.0026585909072309732, -.00042083300650119781],
    "shibata-16000": [-.37251132726669312, -.81423574686050415, -.55010956525802612, -.47405767440795898, -.32624706625938416, -.3161766529083252, -.2286367267370224, -.22916607558727264, -.19565616548061371, -.18160104751586914, -.15423151850700378, -.14104481041431427, -.11844276636838913, -.097583092749118805, -.076493598520755768, -.068106919527053833, -.041881654411554337, -.036922425031661987, -.019364040344953537, -.014994367957115173],
    "shibata-11025": [-.9264228343963623, -.98695987462997437, -.631156325340271, -.51966935396194458, -.39738872647285461, -.35679301619529724, -.29720726609230042, -.26310476660728455, -.21719355881214142, -.18561814725399017, -.15404847264289856, -.12687471508979797, -.10339745879173279, -.083688631653785706, -.05875682458281517, -.046893671154975891, -.027950936928391457, -.020740609616041183, -.009366452693939209, -.0060260160826146603],
    "shibata-8000": [-1.202863335609436, -.94103097915649414, -.67878556251525879, -.57650017738342285, -.50004476308822632, -.44349345564842224, -.37833768129348755, -.34028723835945129, -.29413089156150818, -.24994957447052002, -.21715600788593292, -.18792112171649933, -.15268312394618988, -.12135542929172516, -.099610626697540283, -.075273610651493073, -.048787496984004974, -.042586319148540497, -.028991291299462318, -.011869125068187714],
    "low-shibata-48000": [2.3925774097442626953, -3.4350297451019287109, 3.1853709220886230469, -1.8117271661758422852, -.20124770700931549072, 1.4759907722473144531, -1.7210904359817504883, .97746700048446655273, -.13790138065814971924, -.38185903429985046387, .27421241998672485352, .066584214568138122559, -.35223302245140075684, .37672343850135803223, -.23964276909828186035, .068674825131893157959],
    "low-shibata-44100": [2.0833916664123535156, -3.0418450832366943359, 3.2047898769378662109, -2.7571926116943359375, 1.4978630542755126953, -.3427594602108001709, -.71733748912811279297, 1.0737057924270629883, -1.0225815773010253906, .56649994850158691406, -.20968692004680633545, -.065378531813621520996, .10322438180446624756, -.067442022264003753662, -.00495197344571352005],
    "high-shibata-44100": [3.0259189605712890625, -6.0268716812133789062, 9.195003509521484375, -11.824929237365722656, 12.767142295837402344, -11.917946815490722656, 9.1739168167114257812, -5.3712320327758789062, 1.1393624544143676758, 2.4484779834747314453, -4.9719839096069335938, 6.0392003059387207031, -5.9359521865844726562, 4.903278350830078125, -3.5527443885803222656, 2.1909697055816650391, -1.1672389507293701172, .4903914332389831543, -.16519790887832641602, .023217858746647834778],
}
EXPECTED = {
    "lipshitz-44100": 15, "f-weighted-46000": 34,
    "modified-e-weighted-46000": 8, "improved-e-weighted-46000": 59,
    "shibata-48000": 50, "shibata-44100": 84, "shibata-37800": 26,
    "shibata-32000": 16, "shibata-22050": 6, "shibata-16000": 9,
    "shibata-11025": 10, "shibata-8000": 12,
    "low-shibata-48000": 28, "low-shibata-44100": 27,
    "high-shibata-44100": 155,
}
GESEMANN = {
    "gesemann-44100": [2.2061, -.4706, -.2534, -.6214, 1.0587, .0676, -.6054, -.2738],
    "gesemann-48000": [2.2374, -.7339, -.1251, -.6033, .903, .0116, -.5853, -.2571],
}



def parse_headroom_half_coefficients(path: pathlib.Path):
    text = path.read_text(encoding="utf-8")
    body = text.split("[", 2)[-1].rsplit("]", 1)[0]
    values = [float(token) for token in re.findall(r"[-+]?\d+\.\d+e[-+]\d+", body, re.I)]
    if len(values) != HALF_DELAY_TAPS // 2:
        raise RuntimeError(f"expected 192 checked-in Headroom coefficients, found {len(values)}")
    return values + list(reversed(values))


def generic_two_x(taps: int):
    center = (taps - 1) / 2.0
    h = []
    for tap in range(taps):
        x = (tap - center) / 2.0
        sinc = 1.0 if x == 0.0 else math.sin(math.pi * x) / (math.pi * x)
        phase = tap / (taps - 1)
        window = 0.42 - 0.5 * math.cos(2.0 * math.pi * phase) + 0.08 * math.cos(4.0 * math.pi * phase)
        h.append(sinc * window)
    for branch in range(2):
        total = sum(h[branch::2])
        for tap in range(branch, taps, 2):
            h[tap] /= total
    return h


def convolve(a, b):
    out = [0.0] * (len(a) + len(b) - 1)
    # Put the shorter vector in the inner loop. The Headroom cascade remains
    # small enough that this stdlib-only qualification is comfortably fast.
    if len(a) < len(b):
        a, b = b, a
    for i, x in enumerate(a):
        if x == 0.0:
            continue
        for j, y in enumerate(b):
            if y != 0.0:
                out[i + j] += x * y
    return out


def build_headroom_impulse(half_delay):
    stage1 = [0.0] * (2 * HALF_DELAY_TAPS + 1)
    stage1[HALF_DELAY_TAPS] = 1.0
    stage1[1 : 2 * HALF_DELAY_TAPS : 2] = half_delay
    filters = [stage1] + [generic_two_x(taps) for taps in LATER_TAPS]
    response = [1.0]
    for h in filters:
        up = [0.0] * (2 * len(response) - 1)
        up[::2] = response
        response = convolve(up, h)
    return response


def polyphase_l1(response):
    values = [sum(abs(v) for v in response[phase::OVERSAMPLE]) for phase in range(OVERSAMPLE)]
    phase = max(range(OVERSAMPLE), key=values.__getitem__)
    return values[phase], phase


def convolved_polyphase_l1(headroom_impulse, original_rate_transfer):
    # Upsampling an original-rate error transfer by 64 does not mix 64x
    # phases. Therefore each reconstructed phase is simply the convolution of
    # one Headroom polyphase sequence with the original-rate transfer. This is
    # equivalent to constructing the enormous sparse 64x convolution, without
    # either memory blow-up or a dependency on NumPy/SciPy.
    best = -1.0
    best_phase = 0
    for phase in range(OVERSAMPLE):
        phase_response = headroom_impulse[phase::OVERSAMPLE]
        value = sum(abs(v) for v in convolve(phase_response, original_rate_transfer))
        if value > best:
            best = value
            best_phase = phase
    return best, best_phase


def reconstructed_support_lsb(headroom_impulse, transfer, omitted_transfer_l1=0.0):
    combined_l1, phase = convolved_polyphase_l1(headroom_impulse, transfer)
    # For a truncated stable-IIR transfer, the omitted original-rate L1 mass
    # can contribute at most ||H||_inf times that mass after reconstruction.
    combined_upper = combined_l1 + HEADROOM_RECONSTRUCTION_LINF_UPPER * omitted_transfer_l1
    support = 1.5 * combined_upper
    return support, {"combined_l1": combined_l1, "phase": phase, "omitted_transfer_l1_upper": omitted_transfer_l1}


def matrix_mul(a, b):
    return [[sum(x*y for x, y in zip(row, col)) for col in zip(*b)] for row in a]


def matrix_power(a, n):
    out = [[1.0 if i == j else 0.0 for j in range(len(a))] for i in range(len(a))]
    base = a
    while n:
        if n & 1:
            out = matrix_mul(out, base)
        base = matrix_mul(base, base)
        n >>= 1
    return out


def norm_inf(a):
    return max(sum(abs(v) for v in row) for row in a)


def gesemann_transfer_and_tail(coefs):
    a, b = coefs[:4], coefs[4:]
    errors = [0.0] * 4
    outputs = [0.0] * 4
    transfer = []
    output_partial = 0.0
    for n in range(512):
        e = 1.0 if n == 0 else 0.0
        output = sum(a[j] * errors[j] for j in range(4)) - sum(b[j] * outputs[j] for j in range(4))
        # Stored perturbation relative to the undithered input is e - output.
        transfer.append(e - output)
        output_partial += abs(output)
        errors = [e] + errors[:-1]
        outputs = [output] + outputs[:-1]

    transition = [
        [-b[0], -b[1], -b[2], -b[3]],
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]
    block = None
    for k in range(1, 65):
        q = norm_inf(matrix_power(transition, k))
        if q < 0.5:
            block = (k, q)
            break
    if block is None:
        raise RuntimeError("Gesemann recurrence failed contraction check")
    k, q = block
    within = sum(norm_inf(matrix_power(transition, j)) for j in range(1, k + 1))
    state_norm = max(abs(v) for v in outputs)
    tail = within * state_norm / (1.0 - q)
    # Tiny explicit floating audit allowance; production rounds the final
    # reconstructed multiplier upward by much more than this amount.
    tail += 1e-12
    output_l1_upper = output_partial + tail
    return transfer, tail, output_l1_upper, {"block": k, "q": q, "tail": tail}


def gesemann_output_l1_upper(coefs):
    _, _, output_l1_upper, proof = gesemann_transfer_and_tail(coefs)
    return output_l1_upper, proof


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--repo-root",
        type=pathlib.Path,
        default=pathlib.Path(__file__).resolve().parents[2],
        help="Tonepoet repository root",
    )
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args()

    repo_root = args.repo_root.resolve()
    half_delay = parse_headroom_half_coefficients(
        repo_root / "crates" / "tonepoet-true-peak" / "src" / "headroom64_coefficients.rs"
    )
    headroom_impulse = build_headroom_impulse(half_delay)
    headroom_linf, headroom_phase = polyphase_l1(headroom_impulse)
    if headroom_linf > HEADROOM_RECONSTRUCTION_LINF_UPPER:
        raise RuntimeError(
            f"Headroom reconstruction L-inf {headroom_linf:.17g} exceeds frozen upper "
            f"{HEADROOM_RECONSTRUCTION_LINF_UPPER:.17g}"
        )

    fir_report = {}
    for name, coefs in FIR.items():
        l1 = sum(abs(v) for v in coefs)
        support = 1.5 * (1.0 + l1)
        ceiling = math.ceil(support)
        if ceiling != EXPECTED[name]:
            raise RuntimeError(f"{name}: expected {EXPECTED[name]}, derived {ceiling}")
        transfer = [1.0] + [-value for value in coefs]
        reconstructed, reconstructed_proof = reconstructed_support_lsb(
            headroom_impulse, transfer
        )
        if reconstructed > EXPECTED_INTERIOR_RECONSTRUCTED[name]:
            raise RuntimeError(
                f"{name}: reconstructed support {reconstructed:.17g} exceeds frozen "
                f"{EXPECTED_INTERIOR_RECONSTRUCTED[name]:.17g}"
            )
        fir_report[name] = {
            "coefficient_l1": l1,
            "support_lsb": support,
            "ceiling_lsb": ceiling,
            "interior_lti_reconstructed_support_lsb": reconstructed,
            "interior_lti_reconstructed_ceiling_lsb": EXPECTED_INTERIOR_RECONSTRUCTED[name],
            "interior_lti_reconstructed_proof": reconstructed_proof,
        }

    iir_report = {}
    for name, coefs in GESEMANN.items():
        transfer, transfer_tail, output_l1, proof = gesemann_transfer_and_tail(coefs)
        support = 1.5 * (1.0 + output_l1)
        if support >= 22.0:
            raise RuntimeError(f"{name}: 22-LSB production bound not established: {support}")
        reconstructed, reconstructed_proof = reconstructed_support_lsb(
            headroom_impulse, transfer, transfer_tail
        )
        if reconstructed > EXPECTED_INTERIOR_RECONSTRUCTED[name]:
            raise RuntimeError(
                f"{name}: reconstructed support {reconstructed:.17g} exceeds frozen "
                f"{EXPECTED_INTERIOR_RECONSTRUCTED[name]:.17g}"
            )
        iir_report[name] = {
            "output_l1_upper": output_l1,
            "support_lsb_upper": support,
            "interior_lti_reconstructed_support_lsb_upper": reconstructed,
            "interior_lti_reconstructed_ceiling_lsb": EXPECTED_INTERIOR_RECONSTRUCTED[name],
            "interior_lti_reconstructed_proof": reconstructed_proof,
            **proof,
        }

    # Selection behavior frozen from start_dither(): first same-name table
    # within 5%, otherwise plain/sloped TPDF. In flow_no_shape the random
    # term is within one target LSB and nearest rounding adds at most half an
    # LSB, so these high rates use a 1.5-LSB deterministic support bound.
    for rate in (88200, 96000, 176400, 192000, 352800, 384000):
        if any(abs(rate - design) / design <= .05 for design in (48000, 44100, 37800, 32000, 22050, 16000, 11025, 8000)):
            raise RuntimeError(f"unexpected classic Shibata match at {rate}")

    report = {
        "pinned_sox_ng_revision": "324b8cf873fd7836e8848bd87f7a90d8faa6f849",
        "headroom_reconstruction_linf": {
            "derived": headroom_linf,
            "phase": headroom_phase,
            "frozen_upper": HEADROOM_RECONSTRUCTION_LINF_UPPER,
        },
        "fir": fir_report,
        "gesemann": iir_report,
        "high_rate_classic_shaper_fallback_lsb": 1.5,
        "repeat_endpoints_production_rule": "stored_sample_error_lsb * headroom_reconstruction_linf_upper",
        "high_rate_classic_shaper_repeat_edge_safe_lsb": 1.5 * HEADROOM_RECONSTRUCTION_LINF_UPPER,
    }
    text = json.dumps(report, indent=2, sort_keys=True)
    if args.report:
        args.report.write_text(text + "\n", encoding="utf-8")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
