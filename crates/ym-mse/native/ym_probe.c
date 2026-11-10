/* Library scan metadata, probed in-process through libavformat. */
#include <libavformat/avformat.h>
#include <libavutil/dict.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    char title[512];
    char artist[512];
    char album[512];
    char genre[256];
    char date[64];
    char track[64];
    char audio_codec[64];
    char video_codec[64];
    char subtitle_codec[64];
    char format[128];
    long long duration_ms; /* -1 unknown */
    long long bitrate;     /* -1 unknown */
    int width, height;     /* 0 without video */
    int thumbnail_stream;  /* attached picture stream index, -1 none */
    int has_audio;
} YmProbe;

static void copy_tag(AVDictionary *dict, const char *key, char *out, size_t cap)
{
    AVDictionaryEntry *entry = av_dict_get(dict, key, NULL, 0);
    if (entry && entry->value && !out[0]) {
        strncpy(out, entry->value, cap - 1);
        out[cap - 1] = 0;
    }
}

static void copy_name(const char *name, char *out, size_t cap)
{
    if (name && !out[0]) {
        strncpy(out, name, cap - 1);
        out[cap - 1] = 0;
    }
}

/* 0 on success; a negative AVERROR when the file cannot be opened or read as media. */
int ym_probe(const char *path, YmProbe *out)
{
    AVFormatContext *ctx = NULL;
    int rc;
    memset(out, 0, sizeof(*out));
    out->duration_ms = -1;
    out->bitrate = -1;
    out->thumbnail_stream = -1;
    rc = avformat_open_input(&ctx, path, NULL, NULL);
    if (rc < 0)
        return rc;
    rc = avformat_find_stream_info(ctx, NULL);
    if (rc < 0) {
        avformat_close_input(&ctx);
        return rc;
    }
    copy_name(ctx->iformat ? ctx->iformat->name : NULL, out->format, sizeof(out->format));
    copy_tag(ctx->metadata, "title", out->title, sizeof(out->title));
    copy_tag(ctx->metadata, "artist", out->artist, sizeof(out->artist));
    copy_tag(ctx->metadata, "album_artist", out->artist, sizeof(out->artist));
    copy_tag(ctx->metadata, "album", out->album, sizeof(out->album));
    copy_tag(ctx->metadata, "genre", out->genre, sizeof(out->genre));
    copy_tag(ctx->metadata, "date", out->date, sizeof(out->date));
    copy_tag(ctx->metadata, "year", out->date, sizeof(out->date));
    copy_tag(ctx->metadata, "track", out->track, sizeof(out->track));
    if (ctx->duration > 0)
        out->duration_ms = ctx->duration / 1000;
    if (ctx->bit_rate > 0)
        out->bitrate = ctx->bit_rate;
    for (unsigned i = 0; i < ctx->nb_streams; i++) {
        AVStream *stream = ctx->streams[i];
        AVCodecParameters *par = stream->codecpar;
        const char *name = avcodec_get_name(par->codec_id);
        if (stream->disposition & AV_DISPOSITION_ATTACHED_PIC) {
            if (out->thumbnail_stream < 0)
                out->thumbnail_stream = (int)i;
            continue;
        }
        switch (par->codec_type) {
        case AVMEDIA_TYPE_AUDIO:
            out->has_audio = 1;
            copy_name(name, out->audio_codec, sizeof(out->audio_codec));
            /* Stream tags fill what the container lacks (Ogg/Opus keep them per stream). */
            copy_tag(stream->metadata, "title", out->title, sizeof(out->title));
            copy_tag(stream->metadata, "artist", out->artist, sizeof(out->artist));
            copy_tag(stream->metadata, "album", out->album, sizeof(out->album));
            break;
        case AVMEDIA_TYPE_VIDEO:
            copy_name(name, out->video_codec, sizeof(out->video_codec));
            out->width = par->width;
            out->height = par->height;
            break;
        case AVMEDIA_TYPE_SUBTITLE:
            copy_name(name, out->subtitle_codec, sizeof(out->subtitle_codec));
            break;
        default:
            break;
        }
    }
    avformat_close_input(&ctx);
    return 0;
}
