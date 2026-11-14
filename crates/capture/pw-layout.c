/* Sizes, offsets and constants of what src/pipewire.rs writes by hand, for
 * its `layouts_match_pipewire` test (CI, on Linux, with libpipewire-0.3-dev):
 *
 *   cc $(pkg-config --cflags libpipewire-0.3) crates/capture/pw-layout.c -o pw-layout
 *   ./pw-layout > pw-layout.txt
 *   HEXT_PW_LAYOUT=$PWD/pw-layout.txt cargo test -p hext-capture -- --ignored layouts
 */
#include <stddef.h>
#include <stdio.h>
#include <pipewire/pipewire.h>
#include <spa/param/buffers.h>
#include <spa/param/video/format-utils.h>

#define SIZE(t) printf("%s %zu\n", #t, sizeof(struct t))
#define AT(t, f) printf("%s.%s %zu\n", #t, #f, offsetof(struct t, f))
#define VALUE(c) printf("%s %ld\n", #c, (long)(c))

int main(void) {
    SIZE(spa_hook);
    SIZE(pw_stream_events);
    AT(pw_stream_events, state_changed);
    AT(pw_stream_events, param_changed);
    AT(pw_stream_events, process);
    SIZE(spa_buffer);
    AT(spa_buffer, datas);
    SIZE(spa_data);
    AT(spa_data, maxsize);
    AT(spa_data, data);
    AT(spa_data, chunk);
    SIZE(spa_chunk);
    AT(spa_chunk, stride);
    AT(pw_buffer, buffer);

    VALUE(PW_VERSION_STREAM_EVENTS);
    VALUE(PW_STREAM_FLAG_AUTOCONNECT);
    VALUE(PW_STREAM_FLAG_MAP_BUFFERS);
    VALUE(PW_STREAM_STATE_ERROR);
    VALUE(PW_STREAM_STATE_UNCONNECTED);
    VALUE(PW_STREAM_STATE_STREAMING);
    VALUE(PW_DIRECTION_INPUT);
    VALUE(SPA_TYPE_Id);
    VALUE(SPA_TYPE_Int);
    VALUE(SPA_TYPE_Rectangle);
    VALUE(SPA_TYPE_Fraction);
    VALUE(SPA_TYPE_Object);
    VALUE(SPA_TYPE_Choice);
    VALUE(SPA_TYPE_OBJECT_Format);
    VALUE(SPA_TYPE_OBJECT_ParamBuffers);
    VALUE(SPA_CHOICE_None);
    VALUE(SPA_CHOICE_Range);
    VALUE(SPA_CHOICE_Enum);
    VALUE(SPA_CHOICE_Flags);
    VALUE(SPA_PARAM_EnumFormat);
    VALUE(SPA_PARAM_Format);
    VALUE(SPA_PARAM_Buffers);
    VALUE(SPA_PARAM_BUFFERS_dataType);
    VALUE(SPA_FORMAT_mediaType);
    VALUE(SPA_FORMAT_mediaSubtype);
    VALUE(SPA_FORMAT_VIDEO_format);
    VALUE(SPA_FORMAT_VIDEO_size);
    VALUE(SPA_FORMAT_VIDEO_framerate);
    VALUE(SPA_MEDIA_TYPE_video);
    VALUE(SPA_MEDIA_SUBTYPE_raw);
    VALUE(SPA_VIDEO_FORMAT_RGBx);
    VALUE(SPA_VIDEO_FORMAT_BGRx);
    VALUE(SPA_VIDEO_FORMAT_RGBA);
    VALUE(SPA_VIDEO_FORMAT_BGRA);
    VALUE(SPA_DATA_MemPtr);
    VALUE(SPA_DATA_MemFd);
    VALUE(SPA_CHUNK_FLAG_CORRUPTED);
    return 0;
}
