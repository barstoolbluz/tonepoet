// Test-only DVD-Audio LPCM reference vector generator.
//
// This fixture is derived from foo_input_dvda's pcm_audio_stream_t decoding
// model and audio_stream_info_t MLP/PCM channel-assignment table. It is used
// only by Rust tests to compare Tone Poet's LPCM unpacker against the reference
// algorithm on deterministic pseudo-random raw LPCM payloads.

#include <array>
#include <cstdint>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <string>
#include <vector>

struct Assignment {
    std::vector<std::string> group1;
    std::vector<std::string> group2;
};

static const std::array<Assignment, 21> kAssignments = {{
    Assignment{{"C"}, {}},
    Assignment{{"L", "R"}, {}},
    Assignment{{"L", "R"}, {"S"}},
    Assignment{{"L", "R"}, {"Ls", "Rs"}},
    Assignment{{"L", "R"}, {"LFE"}},
    Assignment{{"L", "R"}, {"LFE", "S"}},
    Assignment{{"L", "R"}, {"LFE", "Ls", "Rs"}},
    Assignment{{"L", "R"}, {"C"}},
    Assignment{{"L", "R"}, {"C", "S"}},
    Assignment{{"L", "R"}, {"C", "Ls", "Rs"}},
    Assignment{{"L", "R"}, {"C", "LFE"}},
    Assignment{{"L", "R"}, {"C", "LFE", "S"}},
    Assignment{{"L", "R"}, {"C", "LFE", "Ls", "Rs"}},
    Assignment{{"L", "R", "C"}, {"S"}},
    Assignment{{"L", "R", "C"}, {"Ls", "Rs"}},
    Assignment{{"L", "R", "C"}, {"LFE"}},
    Assignment{{"L", "R", "C"}, {"LFE", "S"}},
    Assignment{{"L", "R", "C"}, {"LFE", "Ls", "Rs"}},
    Assignment{{"L", "R", "Ls", "Rs"}, {"LFE"}},
    Assignment{{"L", "R", "Ls", "Rs"}, {"C"}},
    Assignment{{"L", "R", "Ls", "Rs"}, {"C", "LFE"}},
}};

struct Lcg {
    uint32_t state;

    explicit Lcg(uint32_t seed) : state(seed) {}

    uint8_t next_u8() {
        state = state * 1664525u + 1013904223u;
        return static_cast<uint8_t>(state >> 24);
    }
};

static size_t raw_group_size(size_t channels, int bits) {
    return channels * static_cast<size_t>(bits) / 4u;
}

static void append_random_bytes(std::vector<uint8_t>& out, size_t count, Lcg& rng) {
    for (size_t i = 0; i < count; ++i) {
        out.push_back(rng.next_u8());
    }
}

static std::vector<uint8_t> make_payload(
    uint8_t assignment,
    int group1_bits,
    int group2_bits,
    int ratio,
    uint32_t seed)
{
    const Assignment& layout = kAssignments.at(assignment);
    const size_t group1_size = raw_group_size(layout.group1.size(), group1_bits);
    const size_t group2_size = raw_group_size(layout.group2.size(), group2_bits);
    const int steps = layout.group2.empty() ? 4 : ratio + 1;
    int raw_group2_index = 0;
    Lcg rng(seed);
    std::vector<uint8_t> payload;

    for (int step = 0; step < steps; ++step) {
        if (!layout.group2.empty() && raw_group2_index == 0) {
            append_random_bytes(payload, group2_size, rng);
        }
        append_random_bytes(payload, group1_size, rng);
        if (!layout.group2.empty()) {
            ++raw_group2_index;
            if (raw_group2_index == ratio) {
                raw_group2_index = 0;
            }
        }
    }
    return payload;
}

static std::vector<int32_t> decode_group(
    const uint8_t* block,
    size_t channels,
    int bits,
    bool group2)
{
    const size_t sample_count = 2 * channels;
    std::vector<int32_t> samples;
    samples.reserve(sample_count);

    for (size_t i = 0; i < sample_count; ++i) {
        const uint8_t hi0 = block[2 * i];
        const uint8_t hi1 = block[2 * i + 1];
        uint8_t byte0 = 0;
        uint8_t byte1 = 0;
        uint8_t byte2 = hi1;
        uint8_t byte3 = hi0;

        if (bits == 16) {
            int16_t value = static_cast<int16_t>((static_cast<uint16_t>(hi0) << 8) | hi1);
            samples.push_back(static_cast<int32_t>(value) << 16);
            continue;
        }

        const size_t packed_offset = 4 * channels;
        if (bits == 20) {
            const uint8_t packed = block[packed_offset + i / 2];
            if (!group2) {
                byte1 = (i % 2 == 0) ? (packed & 0xf0u) : static_cast<uint8_t>(packed << 4);
            } else {
                byte1 = (i % 2 == 0) ? static_cast<uint8_t>(packed >> 4) : (packed & 0x0fu);
            }
        } else if (bits == 24) {
            byte1 = block[packed_offset + i];
        }

        uint32_t value = static_cast<uint32_t>(byte0)
            | (static_cast<uint32_t>(byte1) << 8)
            | (static_cast<uint32_t>(byte2) << 16)
            | (static_cast<uint32_t>(byte3) << 24);
        samples.push_back(static_cast<int32_t>(value));
    }
    return samples;
}

static std::vector<std::string> source_order(const Assignment& layout) {
    std::vector<std::string> order;
    order.reserve(layout.group1.size() + layout.group2.size());
    order.insert(order.end(), layout.group1.begin(), layout.group1.end());
    order.insert(order.end(), layout.group2.begin(), layout.group2.end());
    return order;
}

static std::vector<size_t> wave_indices(const Assignment& layout) {
    std::vector<std::string> source = source_order(layout);
    std::vector<std::string> target;
    for (const char* name : {"L", "R", "C", "LFE", "Ls", "Rs", "S"}) {
        for (const std::string& candidate : source) {
            if (candidate == name) {
                target.emplace_back(name);
                break;
            }
        }
    }
    for (const std::string& name : source) {
        bool present = false;
        for (const std::string& candidate : target) {
            if (candidate == name) {
                present = true;
                break;
            }
        }
        if (!present) {
            target.push_back(name);
        }
    }

    std::vector<size_t> indices;
    indices.reserve(target.size());
    for (const std::string& name : target) {
        for (size_t i = 0; i < source.size(); ++i) {
            if (source[i] == name) {
                indices.push_back(i);
                break;
            }
        }
    }
    return indices;
}

static void append_i32le(std::vector<uint8_t>& out, int32_t value) {
    uint32_t u = static_cast<uint32_t>(value);
    out.push_back(static_cast<uint8_t>(u & 0xffu));
    out.push_back(static_cast<uint8_t>((u >> 8) & 0xffu));
    out.push_back(static_cast<uint8_t>((u >> 16) & 0xffu));
    out.push_back(static_cast<uint8_t>((u >> 24) & 0xffu));
}

static void append_frame(
    std::vector<uint8_t>& out,
    const std::vector<int32_t>& frame,
    const std::vector<size_t>& order)
{
    for (size_t index : order) {
        append_i32le(out, frame.at(index));
    }
}

struct DecodeResult {
    std::vector<uint8_t> source_s32le;
    std::vector<uint8_t> wave_s32le;
};

static DecodeResult decode_reference(
    uint8_t assignment,
    int group1_bits,
    int group2_bits,
    int ratio,
    const std::vector<uint8_t>& payload)
{
    const Assignment& layout = kAssignments.at(assignment);
    const size_t group1_channels = layout.group1.size();
    const size_t group2_channels = layout.group2.size();
    const size_t raw_group1_size = raw_group_size(group1_channels, group1_bits);
    const size_t raw_group2_size = raw_group_size(group2_channels, group2_bits);
    const std::vector<size_t> source_indices = [&] {
        std::vector<size_t> indices(group1_channels + group2_channels);
        for (size_t i = 0; i < indices.size(); ++i) {
            indices[i] = i;
        }
        return indices;
    }();
    const std::vector<size_t> wave = wave_indices(layout);

    DecodeResult result;
    std::vector<int32_t> last_group2(2 * group2_channels, 0);
    int raw_group2_index = 0;
    size_t offset = 0;

    while (offset + raw_group1_size + ((!layout.group2.empty() && raw_group2_index == 0) ? raw_group2_size : 0) <= payload.size()) {
        std::vector<int32_t> group2 = last_group2;
        if (!layout.group2.empty() && raw_group2_index == 0) {
            group2 = decode_group(payload.data() + offset, group2_channels, group2_bits, true);
            last_group2 = group2;
            offset += raw_group2_size;
        }
        if (!layout.group2.empty()) {
            ++raw_group2_index;
            if (raw_group2_index == ratio) {
                raw_group2_index = 0;
            }
        }

        std::vector<int32_t> group1 = decode_group(payload.data() + offset, group1_channels, group1_bits, false);
        offset += raw_group1_size;

        std::vector<int32_t> first_frame;
        first_frame.reserve(group1_channels + group2_channels);
        first_frame.insert(first_frame.end(), group1.begin(), group1.begin() + static_cast<std::ptrdiff_t>(group1_channels));
        first_frame.insert(first_frame.end(), group2.begin(), group2.begin() + static_cast<std::ptrdiff_t>(group2_channels));
        append_frame(result.source_s32le, first_frame, source_indices);
        append_frame(result.wave_s32le, first_frame, wave);

        std::vector<int32_t> second_frame;
        second_frame.reserve(group1_channels + group2_channels);
        second_frame.insert(second_frame.end(), group1.begin() + static_cast<std::ptrdiff_t>(group1_channels), group1.end());
        second_frame.insert(second_frame.end(), group2.begin() + static_cast<std::ptrdiff_t>(group2_channels), group2.end());
        append_frame(result.source_s32le, second_frame, source_indices);
        append_frame(result.wave_s32le, second_frame, wave);
    }

    return result;
}

static std::string hex(const std::vector<uint8_t>& bytes) {
    std::ostringstream os;
    os << std::hex << std::setfill('0');
    for (uint8_t byte : bytes) {
        os << std::setw(2) << static_cast<unsigned>(byte);
    }
    return os.str();
}

int main() {
    constexpr std::array<int, 3> kBits = {{16, 20, 24}};
    constexpr std::array<int, 3> kRatios = {{1, 2, 4}};
    constexpr int kGroup1Rate = 192000;
    uint64_t emitted = 0;

    std::cout << "# foo_input_dvda LPCM reference vectors\n";
    for (uint8_t assignment = 0; assignment < kAssignments.size(); ++assignment) {
        const Assignment& layout = kAssignments.at(assignment);
        for (int group1_bits : kBits) {
            if (layout.group2.empty()) {
                const int group2_bits = 0;
                const int ratio = 1;
                const uint32_t seed = 0x44564441u ^ (static_cast<uint32_t>(assignment) << 24)
                    ^ (static_cast<uint32_t>(group1_bits) << 8);
                std::vector<uint8_t> payload = make_payload(assignment, group1_bits, 16, ratio, seed);
                DecodeResult decoded = decode_reference(assignment, group1_bits, 16, ratio, payload);
                std::cout
                    << "code=" << static_cast<int>(assignment)
                    << "\tg1_bits=" << group1_bits
                    << "\tg2_bits=" << group2_bits
                    << "\tg1_rate=" << kGroup1Rate
                    << "\tg2_rate=0"
                    << "\tratio=" << ratio
                    << "\tpayload=" << hex(payload)
                    << "\tsource_s32le=" << hex(decoded.source_s32le)
                    << "\twave_s32le=" << hex(decoded.wave_s32le)
                    << "\n";
                ++emitted;
                continue;
            }

            for (int group2_bits : kBits) {
                // foo_input_dvda sizes its output sample container from group 1.
                // DVD-A group 2 is expected to be no deeper than group 1; vectors
                // with a deeper group 2 would exercise truncation behavior rather
                // than valid DVD-A sample packing.
                if (group2_bits > group1_bits) {
                    continue;
                }
                for (int ratio : kRatios) {
                    const int group2_rate = kGroup1Rate / ratio;
                    const uint32_t seed = 0x44564441u ^ (static_cast<uint32_t>(assignment) << 24)
                        ^ (static_cast<uint32_t>(group1_bits) << 12)
                        ^ (static_cast<uint32_t>(group2_bits) << 4)
                        ^ static_cast<uint32_t>(ratio);
                    std::vector<uint8_t> payload = make_payload(assignment, group1_bits, group2_bits, ratio, seed);
                    DecodeResult decoded = decode_reference(assignment, group1_bits, group2_bits, ratio, payload);
                    std::cout
                        << "code=" << static_cast<int>(assignment)
                        << "\tg1_bits=" << group1_bits
                        << "\tg2_bits=" << group2_bits
                        << "\tg1_rate=" << kGroup1Rate
                        << "\tg2_rate=" << group2_rate
                        << "\tratio=" << ratio
                        << "\tpayload=" << hex(payload)
                        << "\tsource_s32le=" << hex(decoded.source_s32le)
                        << "\twave_s32le=" << hex(decoded.wave_s32le)
                        << "\n";
                    ++emitted;
                }
            }
        }
    }
    std::cout << "# vectors=" << emitted << "\n";
    return 0;
}
