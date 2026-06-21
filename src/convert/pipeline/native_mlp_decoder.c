#include "native_mlp_decoder.h"

#include <errno.h>
#include <inttypes.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavutil/channel_layout.h>
#include <libavutil/error.h>
#include <libavutil/mem.h>
#include <libavutil/opt.h>
#include <libavutil/samplefmt.h>
#include <libavutil/version.h>
#include <libswresample/swresample.h>

#define INPUT_CHUNK_SIZE 32768
#define WAV_HEADER_SIZE 44


/*
 * FFmpeg 7.1 libavcodec/mlpdec.c defines the private decoder context prefix as:
 *
 *   const AVClass *class;
 *   AVCodecContext *avctx;
 *   AVChannelLayout downmix_layout;
 *
 * The public AVOption table exposes the byte offset for `downmix`.  The shim
 * validates that offset against this prefix before writing the field directly.
 * No Rust code reads or writes FFmpeg private memory.
 */
typedef struct tonepoet_ffmpeg_71_mlp_decode_context_prefix {
    const AVClass *class;
    AVCodecContext *avctx;
    AVChannelLayout downmix_layout;
} tonepoet_ffmpeg_71_mlp_decode_context_prefix;

static int tonepoet_expected_downmix_layout_offset(void) {
    return (int)offsetof(tonepoet_ffmpeg_71_mlp_decode_context_prefix, downmix_layout);
}

typedef struct tonepoet_decode_ctx {
    FILE *out_file;
    SwrContext *swr;
    int output_sample_rate;
    int saw_frame;
    uint64_t samples_per_channel;
    uint64_t data_bytes;
    char *error;
    size_t error_cap;
} tonepoet_decode_ctx;

static void tonepoet_set_text(char *dst, size_t dst_cap, const char *fmt, ...) {
    if (dst == NULL || dst_cap == 0) {
        return;
    }
    va_list args;
    va_start(args, fmt);
    vsnprintf(dst, dst_cap, fmt, args);
    va_end(args);
    dst[dst_cap - 1] = '\0';
}

static void tonepoet_set_av_error(char *dst, size_t dst_cap, const char *prefix, int errnum) {
    char errbuf[AV_ERROR_MAX_STRING_SIZE] = {0};
    av_strerror(errnum, errbuf, sizeof(errbuf));
    tonepoet_set_text(dst, dst_cap, "%s: %s", prefix, errbuf);
}

static const AVCodec *tonepoet_find_mlp_decoder(void) {
    return avcodec_find_decoder(AV_CODEC_ID_MLP);
}

static const AVOption *tonepoet_find_downmix_option(AVCodecContext *ctx) {
    if (ctx == NULL) {
        return NULL;
    }
    if (ctx->priv_data != NULL) {
        const AVOption *opt = av_opt_find(ctx->priv_data, "downmix", NULL, 0, AV_OPT_SEARCH_CHILDREN);
        if (opt != NULL) {
            return opt;
        }
    }
    return av_opt_find(ctx, "downmix", NULL, 0, AV_OPT_SEARCH_CHILDREN);
}

static AVChannelLayout *tonepoet_private_mlp_downmix_layout(AVCodecContext *ctx, int *option_offset, int *expected_offset, char *err, size_t err_cap) {
    if (option_offset != NULL) {
        *option_offset = -1;
    }
    if (expected_offset != NULL) {
        *expected_offset = tonepoet_expected_downmix_layout_offset();
    }
    if (ctx == NULL) {
        tonepoet_set_text(err, err_cap, "MLP decoder context is null");
        return NULL;
    }
    if (ctx->priv_data == NULL) {
        tonepoet_set_text(err, err_cap, "MLP decoder private context is null");
        return NULL;
    }

    const AVOption *opt = tonepoet_find_downmix_option(ctx);
    if (opt == NULL) {
        tonepoet_set_text(err, err_cap, "MLP decoder AVOption 'downmix' is not available; cannot locate private downmix_layout field");
        return NULL;
    }

    int opt_offset = opt->offset;
    int expected = tonepoet_expected_downmix_layout_offset();
    if (option_offset != NULL) {
        *option_offset = opt_offset;
    }
    if (opt_offset != expected) {
        tonepoet_set_text(
            err,
            err_cap,
            "MLP decoder private layout check failed: AVOption offset=%d, expected FFmpeg 7.1 downmix_layout offset=%d",
            opt_offset,
            expected);
        return NULL;
    }

    return (AVChannelLayout *)((uint8_t *)ctx->priv_data + opt_offset);
}

static int tonepoet_set_mlp_downmix_stereo_private(
    AVCodecContext *ctx,
    int *option_offset,
    int *expected_offset,
    char *err,
    size_t err_cap) {
    AVChannelLayout *target = tonepoet_private_mlp_downmix_layout(ctx, option_offset, expected_offset, err, err_cap);
    if (target == NULL) {
        return -1;
    }

    av_channel_layout_uninit(target);
    av_channel_layout_default(target, 2);
    if (target->nb_channels != 2) {
        tonepoet_set_text(err, err_cap, "failed to write stereo downmix_layout into MLP decoder private context");
        return -2;
    }
    return 0;
}

static int tonepoet_write_le16(FILE *f, uint16_t value) {
    unsigned char bytes[2];
    bytes[0] = (unsigned char)(value & 0xffu);
    bytes[1] = (unsigned char)((value >> 8) & 0xffu);
    return fwrite(bytes, 1, sizeof(bytes), f) == sizeof(bytes) ? 0 : -1;
}

static int tonepoet_write_le32(FILE *f, uint32_t value) {
    unsigned char bytes[4];
    bytes[0] = (unsigned char)(value & 0xffu);
    bytes[1] = (unsigned char)((value >> 8) & 0xffu);
    bytes[2] = (unsigned char)((value >> 16) & 0xffu);
    bytes[3] = (unsigned char)((value >> 24) & 0xffu);
    return fwrite(bytes, 1, sizeof(bytes), f) == sizeof(bytes) ? 0 : -1;
}

static int tonepoet_write_wav_header(FILE *f, uint32_t sample_rate, uint32_t data_bytes) {
    if (fseek(f, 0, SEEK_SET) != 0) {
        return -1;
    }
    if (fwrite("RIFF", 1, 4, f) != 4) return -1;
    if (tonepoet_write_le32(f, 36u + data_bytes) != 0) return -1;
    if (fwrite("WAVE", 1, 4, f) != 4) return -1;
    if (fwrite("fmt ", 1, 4, f) != 4) return -1;
    if (tonepoet_write_le32(f, 16u) != 0) return -1;
    if (tonepoet_write_le16(f, 1u) != 0) return -1;
    if (tonepoet_write_le16(f, 2u) != 0) return -1;
    if (tonepoet_write_le32(f, sample_rate) != 0) return -1;
    if (tonepoet_write_le32(f, sample_rate * 2u * 4u) != 0) return -1;
    if (tonepoet_write_le16(f, 2u * 4u) != 0) return -1;
    if (tonepoet_write_le16(f, 32u) != 0) return -1;
    if (fwrite("data", 1, 4, f) != 4) return -1;
    if (tonepoet_write_le32(f, data_bytes) != 0) return -1;
    return 0;
}

static int tonepoet_start_wav(FILE *f, char *err, size_t err_cap) {
    if (tonepoet_write_wav_header(f, 48000u, 0u) != 0) {
        tonepoet_set_text(err, err_cap, "failed to write placeholder WAV header");
        return -1;
    }
    return 0;
}

static int tonepoet_finish_wav(tonepoet_decode_ctx *ctx) {
    if (ctx->data_bytes > 0xffffffffu) {
        tonepoet_set_text(ctx->error, ctx->error_cap, "decoded WAV exceeds RIFF 32-bit data limit");
        return -1;
    }
    if (tonepoet_write_wav_header(ctx->out_file, (uint32_t)ctx->output_sample_rate, (uint32_t)ctx->data_bytes) != 0) {
        tonepoet_set_text(ctx->error, ctx->error_cap, "failed to rewrite final WAV header");
        return -1;
    }
    if (fflush(ctx->out_file) != 0) {
        tonepoet_set_text(ctx->error, ctx->error_cap, "failed to flush WAV output: %s", strerror(errno));
        return -1;
    }
    return 0;
}

static int tonepoet_init_swr(tonepoet_decode_ctx *ctx, const AVFrame *frame) {
    if (frame->ch_layout.nb_channels != 2) {
        char layout[128] = {0};
        av_channel_layout_describe(&frame->ch_layout, layout, sizeof(layout));
        tonepoet_set_text(
            ctx->error,
            ctx->error_cap,
            "MLP native stereo decoder emitted %d channels (layout=%s); expected stereo",
            frame->ch_layout.nb_channels,
            layout[0] ? layout : "unknown");
        return -1;
    }

    AVChannelLayout stereo;
    memset(&stereo, 0, sizeof(stereo));
    av_channel_layout_default(&stereo, 2);
    int ret = swr_alloc_set_opts2(
        &ctx->swr,
        &stereo,
        AV_SAMPLE_FMT_S32,
        frame->sample_rate,
        &frame->ch_layout,
        (enum AVSampleFormat)frame->format,
        frame->sample_rate,
        0,
        NULL);
    av_channel_layout_uninit(&stereo);
    if (ret < 0) {
        tonepoet_set_av_error(ctx->error, ctx->error_cap, "failed to allocate libswresample context", ret);
        return ret;
    }
    ret = swr_init(ctx->swr);
    if (ret < 0) {
        tonepoet_set_av_error(ctx->error, ctx->error_cap, "failed to initialize libswresample context", ret);
        return ret;
    }
    ctx->output_sample_rate = frame->sample_rate;
    return 0;
}

static int tonepoet_write_frame(tonepoet_decode_ctx *ctx, const AVFrame *frame) {
    if (!ctx->saw_frame) {
        int ret = tonepoet_init_swr(ctx, frame);
        if (ret < 0) {
            return ret;
        }
        ctx->saw_frame = 1;
    }

    if (frame->sample_rate != ctx->output_sample_rate) {
        tonepoet_set_text(
            ctx->error,
            ctx->error_cap,
            "MLP stream changed sample rate from %d to %d during decode",
            ctx->output_sample_rate,
            frame->sample_rate);
        return -1;
    }
    if (frame->ch_layout.nb_channels != 2) {
        tonepoet_set_text(ctx->error, ctx->error_cap, "MLP native stereo decode emitted a non-stereo frame after startup");
        return -1;
    }

    int delay = (int)swr_get_delay(ctx->swr, frame->sample_rate);
    int out_samples = av_rescale_rnd(delay + frame->nb_samples, frame->sample_rate, frame->sample_rate, AV_ROUND_UP);
    uint8_t **out_data = NULL;
    int out_linesize = 0;
    int ret = av_samples_alloc_array_and_samples(
        &out_data,
        &out_linesize,
        2,
        out_samples,
        AV_SAMPLE_FMT_S32,
        0);
    if (ret < 0) {
        tonepoet_set_av_error(ctx->error, ctx->error_cap, "failed to allocate decoded PCM buffer", ret);
        return ret;
    }

    ret = swr_convert(
        ctx->swr,
        out_data,
        out_samples,
        (const uint8_t **)frame->extended_data,
        frame->nb_samples);
    if (ret < 0) {
        tonepoet_set_av_error(ctx->error, ctx->error_cap, "failed to convert decoded PCM frame to s32le", ret);
        av_freep(&out_data[0]);
        av_freep(&out_data);
        return ret;
    }

    size_t bytes = (size_t)ret * 2u * sizeof(int32_t);
    if (bytes > 0 && fwrite(out_data[0], 1, bytes, ctx->out_file) != bytes) {
        tonepoet_set_text(ctx->error, ctx->error_cap, "failed to write decoded PCM data: %s", strerror(errno));
        av_freep(&out_data[0]);
        av_freep(&out_data);
        return -1;
    }
    ctx->samples_per_channel += (uint64_t)ret;
    ctx->data_bytes += (uint64_t)bytes;

    av_freep(&out_data[0]);
    av_freep(&out_data);
    return 0;
}

static int tonepoet_receive_frames(AVCodecContext *codec_ctx, AVFrame *frame, tonepoet_decode_ctx *decode_ctx) {
    for (;;) {
        int ret = avcodec_receive_frame(codec_ctx, frame);
        if (ret == AVERROR(EAGAIN) || ret == AVERROR_EOF) {
            return 0;
        }
        if (ret < 0) {
            tonepoet_set_av_error(decode_ctx->error, decode_ctx->error_cap, "MLP decoder failed to receive frame", ret);
            return ret;
        }
        ret = tonepoet_write_frame(decode_ctx, frame);
        av_frame_unref(frame);
        if (ret < 0) {
            return ret;
        }
    }
}

static int tonepoet_send_packet(AVCodecContext *codec_ctx, AVPacket *packet, AVFrame *frame, tonepoet_decode_ctx *decode_ctx) {
    int ret = avcodec_send_packet(codec_ctx, packet);
    if (ret < 0) {
        tonepoet_set_av_error(decode_ctx->error, decode_ctx->error_cap, "MLP decoder failed to accept packet", ret);
        return ret;
    }
    return tonepoet_receive_frames(codec_ctx, frame, decode_ctx);
}

int tonepoet_native_mlp_decoder_info(struct tonepoet_native_mlp_decoder_info *out) {
    if (out == NULL) {
        return -1;
    }
    memset(out, 0, sizeof(*out));
    out->downmix_option_offset = -1;
    out->private_downmix_layout_offset = tonepoet_expected_downmix_layout_offset();
    out->avcodec_version = avcodec_version();
    tonepoet_set_text(out->avcodec_version_text, sizeof(out->avcodec_version_text), "%u", out->avcodec_version);
    const char *configuration = avcodec_configuration();
    tonepoet_set_text(out->avcodec_configuration, sizeof(out->avcodec_configuration), "%s", configuration ? configuration : "unknown");

    const AVCodec *codec = tonepoet_find_mlp_decoder();
    if (codec == NULL) {
        tonepoet_set_text(out->error, sizeof(out->error), "libavcodec MLP decoder is not available");
        return 0;
    }
    out->decoder_available = 1;

    AVCodecContext *ctx = avcodec_alloc_context3(codec);
    if (ctx == NULL) {
        tonepoet_set_text(out->error, sizeof(out->error), "failed to allocate MLP decoder context");
        return 0;
    }
    const AVOption *downmix = tonepoet_find_downmix_option(ctx);
    out->downmix_option_available = downmix != NULL;
    out->downmix_option_offset = downmix != NULL ? downmix->offset : -1;
    out->private_downmix_layout_offset = tonepoet_expected_downmix_layout_offset();
    out->private_downmix_layout_available =
        downmix != NULL && out->downmix_option_offset == out->private_downmix_layout_offset;
    if (!out->downmix_option_available) {
        tonepoet_set_text(out->error, sizeof(out->error), "MLP decoder AVOption 'downmix' is not available; cannot locate private downmix_layout field");
    } else if (!out->private_downmix_layout_available) {
        tonepoet_set_text(
            out->error,
            sizeof(out->error),
            "MLP decoder downmix_layout offset mismatch: AVOption offset=%d, expected FFmpeg 7.1 offset=%d",
            out->downmix_option_offset,
            out->private_downmix_layout_offset);
    } else if (tonepoet_set_mlp_downmix_stereo_private(
                   ctx,
                   &out->downmix_option_offset,
                   &out->private_downmix_layout_offset,
                   out->error,
                   sizeof(out->error)) == 0) {
        out->private_downmix_layout_set = 1;
    }
    avcodec_free_context(&ctx);
    return 0;
}

int tonepoet_native_mlp_decode_stereo_s32le_wav(
    const char *input_path,
    const char *output_path,
    struct tonepoet_native_mlp_decode_result *out) {
    if (out == NULL) {
        return -1;
    }
    memset(out, 0, sizeof(*out));
    out->downmix_option_offset = -1;
    out->private_downmix_layout_offset = tonepoet_expected_downmix_layout_offset();
    out->avcodec_version = avcodec_version();

    const AVCodec *codec = tonepoet_find_mlp_decoder();
    if (codec == NULL) {
        tonepoet_set_text(out->error, sizeof(out->error), "libavcodec MLP decoder is not available");
        return -2;
    }

    FILE *input = fopen(input_path, "rb");
    if (input == NULL) {
        tonepoet_set_text(out->error, sizeof(out->error), "failed to open MLP input %s: %s", input_path, strerror(errno));
        return -3;
    }
    FILE *output = fopen(output_path, "wb+");
    if (output == NULL) {
        tonepoet_set_text(out->error, sizeof(out->error), "failed to open WAV output %s: %s", output_path, strerror(errno));
        fclose(input);
        return -4;
    }

    int rc = -1;
    AVCodecParserContext *parser = NULL;
    AVCodecContext *codec_ctx = NULL;
    AVPacket *packet = NULL;
    AVFrame *frame = NULL;
    uint8_t *input_buf = NULL;
    tonepoet_decode_ctx decode_ctx;
    memset(&decode_ctx, 0, sizeof(decode_ctx));
    decode_ctx.out_file = output;
    decode_ctx.error = out->error;
    decode_ctx.error_cap = sizeof(out->error);

    if (tonepoet_start_wav(output, out->error, sizeof(out->error)) != 0) {
        goto done;
    }

    parser = av_parser_init(AV_CODEC_ID_MLP);
    if (parser == NULL) {
        tonepoet_set_text(out->error, sizeof(out->error), "failed to allocate MLP parser");
        goto done;
    }
    codec_ctx = avcodec_alloc_context3(codec);
    if (codec_ctx == NULL) {
        tonepoet_set_text(out->error, sizeof(out->error), "failed to allocate MLP decoder context");
        goto done;
    }
    if (tonepoet_set_mlp_downmix_stereo_private(
            codec_ctx,
            &out->downmix_option_offset,
            &out->private_downmix_layout_offset,
            out->error,
            sizeof(out->error)) < 0) {
        goto done;
    }
    out->private_downmix_layout_set = 1;
    int ret = avcodec_open2(codec_ctx, codec, NULL);
    if (ret < 0) {
        tonepoet_set_av_error(out->error, sizeof(out->error), "failed to open MLP decoder", ret);
        goto done;
    }
    if (tonepoet_set_mlp_downmix_stereo_private(
            codec_ctx,
            &out->downmix_option_offset,
            &out->private_downmix_layout_offset,
            out->error,
            sizeof(out->error)) < 0) {
        goto done;
    }
    out->private_downmix_layout_set = 1;

    packet = av_packet_alloc();
    frame = av_frame_alloc();
    if (packet == NULL || frame == NULL) {
        tonepoet_set_text(out->error, sizeof(out->error), "failed to allocate decoder packet or frame");
        goto done;
    }
    input_buf = av_malloc(INPUT_CHUNK_SIZE + AV_INPUT_BUFFER_PADDING_SIZE);
    if (input_buf == NULL) {
        tonepoet_set_text(out->error, sizeof(out->error), "failed to allocate input buffer");
        goto done;
    }

    for (;;) {
        size_t bytes_read = fread(input_buf, 1, INPUT_CHUNK_SIZE, input);
        if (bytes_read == 0) {
            if (ferror(input)) {
                tonepoet_set_text(out->error, sizeof(out->error), "failed to read MLP input: %s", strerror(errno));
                goto done;
            }
            break;
        }
        memset(input_buf + bytes_read, 0, AV_INPUT_BUFFER_PADDING_SIZE);
        uint8_t *data = input_buf;
        int data_size = (int)bytes_read;
        while (data_size > 0) {
            uint8_t *parsed_data = NULL;
            int parsed_size = 0;
            ret = av_parser_parse2(
                parser,
                codec_ctx,
                &parsed_data,
                &parsed_size,
                data,
                data_size,
                AV_NOPTS_VALUE,
                AV_NOPTS_VALUE,
                0);
            if (ret < 0) {
                tonepoet_set_av_error(out->error, sizeof(out->error), "MLP parser failed", ret);
                goto done;
            }
            data += ret;
            data_size -= ret;
            if (parsed_size > 0) {
                packet->data = parsed_data;
                packet->size = parsed_size;
                if (tonepoet_send_packet(codec_ctx, packet, frame, &decode_ctx) < 0) {
                    goto done;
                }
            }
        }
    }

    ret = avcodec_send_packet(codec_ctx, NULL);
    if (ret < 0 && ret != AVERROR_EOF) {
        tonepoet_set_av_error(out->error, sizeof(out->error), "MLP decoder flush failed", ret);
        goto done;
    }
    if (tonepoet_receive_frames(codec_ctx, frame, &decode_ctx) < 0) {
        goto done;
    }
    if (!decode_ctx.saw_frame) {
        tonepoet_set_text(out->error, sizeof(out->error), "MLP decoder produced no audio frames");
        goto done;
    }
    if (tonepoet_finish_wav(&decode_ctx) != 0) {
        goto done;
    }

    out->channels = 2;
    out->sample_rate = decode_ctx.output_sample_rate;
    out->samples_per_channel = decode_ctx.samples_per_channel;
    out->data_bytes = decode_ctx.data_bytes;
    AVChannelLayout stereo;
    memset(&stereo, 0, sizeof(stereo));
    av_channel_layout_default(&stereo, 2);
    av_channel_layout_describe(&stereo, out->channel_layout, sizeof(out->channel_layout));
    av_channel_layout_uninit(&stereo);
    rc = 0;

done:
    if (input_buf != NULL) av_free(input_buf);
    if (frame != NULL) av_frame_free(&frame);
    if (packet != NULL) av_packet_free(&packet);
    if (codec_ctx != NULL) avcodec_free_context(&codec_ctx);
    if (parser != NULL) av_parser_close(parser);
    if (decode_ctx.swr != NULL) swr_free(&decode_ctx.swr);
    fclose(input);
    if (fclose(output) != 0 && rc == 0) {
        tonepoet_set_text(out->error, sizeof(out->error), "failed to close WAV output: %s", strerror(errno));
        rc = -1;
    }
    if (rc != 0) {
        remove(output_path);
    }
    return rc;
}
