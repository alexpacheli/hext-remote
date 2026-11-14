/* Sizes and offsets of the libva structures src/vaapi.rs builds by hand,
 * for its `layouts_match_libva` test (CI, on Linux, with libva-dev):
 *
 *   cc crates/codec/va-layout.c -o va-layout && ./va-layout > va-layout.txt
 *   HEXT_VA_LAYOUT=$PWD/va-layout.txt cargo test -p hext-codec -- --ignored layouts
 */
#include <stddef.h>
#include <stdio.h>
#include <va/va.h>
#include <va/va_enc_h264.h>

#define SIZE(t) printf("%s %zu\n", #t, sizeof(t))
#define AT(t, f) printf("%s.%s %zu\n", #t, #f, offsetof(t, f))

int main(void) {
    SIZE(VAConfigAttrib);
    SIZE(VAPictureH264);
    SIZE(VAEncSequenceParameterBufferH264);
    AT(VAEncSequenceParameterBufferH264, seq_fields);
    AT(VAEncSequenceParameterBufferH264, frame_cropping_flag);
    AT(VAEncSequenceParameterBufferH264, time_scale);
    SIZE(VAEncPictureParameterBufferH264);
    AT(VAEncPictureParameterBufferH264, coded_buf);
    AT(VAEncPictureParameterBufferH264, frame_num);
    AT(VAEncPictureParameterBufferH264, pic_fields);
    SIZE(VAEncSliceParameterBufferH264);
    AT(VAEncSliceParameterBufferH264, RefPicList0);
    AT(VAEncSliceParameterBufferH264, luma_weight_l0);
    AT(VAEncSliceParameterBufferH264, chroma_weight_l1);
    AT(VAEncSliceParameterBufferH264, cabac_init_idc);
    SIZE(VAEncMiscParameterRateControl);
    SIZE(VAEncMiscParameterFrameRate);
    SIZE(VAEncMiscParameterHRD);
    SIZE(VAImageFormat);
    SIZE(VAImage);
    AT(VAImage, buf);
    AT(VAImage, pitches);
    AT(VAImage, offsets);
    SIZE(VACodedBufferSegment);
    AT(VACodedBufferSegment, buf);
    AT(VACodedBufferSegment, next);
    return 0;
}
