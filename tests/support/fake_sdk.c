/* Test-owned SDK substitute. Its ABI is checked by the installed public headers.
 * No native discovery, network activity, proprietary implementation or SDK data.
 */
#include <Processing.NDI.Lib.h>
#include <stdio.h>
#include <stdlib.h>
#ifndef FAKE_NDI_VERSION
#define FAKE_NDI_VERSION "NDI SDK TEST 6.3.2.0"
#endif
static void fake_log(const char* operation) {
    const char* path = getenv("GRAFTON_FAKE_NDI_LOG");
    if (!path) return;
    FILE* f = fopen(path, "a");
    if (f) { fprintf(f, "%s\n", operation); fclose(f); }
}

#ifdef FAKE_OBJECTS
#include <string.h>
struct fake_sender {
    void* opaque;
    void (*completion)(void*, const NDIlib_video_frame_v2_t*);
    NDIlib_video_frame_v2_t pending[128];
    unsigned count;
};
static void fake_complete(struct fake_sender* sender) {
    while (sender->count) {
        NDIlib_video_frame_v2_t frame = sender->pending[--sender->count];
        fake_log("sender-completion");
        sender->completion(sender->opaque, &frame);
    }
}
#endif

bool NDIlib_initialize(void) {
    fake_log("initialize"); return true;
}

void NDIlib_destroy(void) {
    fake_log("destroy");
}

const char* NDIlib_version(void) {
    return FAKE_NDI_VERSION;
}

bool NDIlib_is_supported_CPU(void) {
    return true;
}

NDIlib_find_instance_t NDIlib_find_create_v2(const NDIlib_find_create_t* p_create_settings) {
    return 0;
}

void NDIlib_find_destroy(NDIlib_find_instance_t p_instance) {
}

const NDIlib_source_t* NDIlib_find_get_current_sources(NDIlib_find_instance_t p_instance, uint32_t* p_no_sources) {
    return 0;
}

bool NDIlib_find_wait_for_sources(NDIlib_find_instance_t p_instance, uint32_t timeout_in_ms) {
    return 0;
}

NDIlib_recv_instance_t NDIlib_recv_create_v3(const NDIlib_recv_create_v3_t* p_create_settings) {
#ifdef FAKE_OBJECTS
    fake_log("receiver-create"); return (NDIlib_recv_instance_t)calloc(1, 1);
#else
    return 0;
#endif
}

void NDIlib_recv_destroy(NDIlib_recv_instance_t p_instance) {
#ifdef FAKE_OBJECTS
    fake_log("receiver-destroy"); free(p_instance);
#endif
}

void NDIlib_recv_connect(NDIlib_recv_instance_t p_instance, const NDIlib_source_t* p_src) {
}

NDIlib_frame_type_e NDIlib_recv_capture_v3(NDIlib_recv_instance_t p_instance, NDIlib_video_frame_v2_t* p_video_data, NDIlib_audio_frame_v3_t* p_audio_data, NDIlib_metadata_frame_t* p_metadata, uint32_t timeout_in_ms) {
    return 0;
}

void NDIlib_recv_free_video_v2(NDIlib_recv_instance_t p_instance, const NDIlib_video_frame_v2_t* p_video_data) {
}

void NDIlib_recv_free_audio_v3(NDIlib_recv_instance_t p_instance, const NDIlib_audio_frame_v3_t* p_audio_data) {
}

void NDIlib_recv_free_metadata(NDIlib_recv_instance_t p_instance, const NDIlib_metadata_frame_t* p_metadata) {
}

bool NDIlib_recv_send_metadata(NDIlib_recv_instance_t p_instance, const NDIlib_metadata_frame_t* p_metadata) {
    return 0;
}

bool NDIlib_recv_set_tally(NDIlib_recv_instance_t p_instance, const NDIlib_tally_t* p_tally) {
    return 0;
}

void NDIlib_recv_get_performance(NDIlib_recv_instance_t p_instance, NDIlib_recv_performance_t* p_total, NDIlib_recv_performance_t* p_dropped) {
}

void NDIlib_recv_get_queue(NDIlib_recv_instance_t p_instance, NDIlib_recv_queue_t* p_total) {
}

int NDIlib_recv_get_no_connections(NDIlib_recv_instance_t p_instance) {
    return 0;
}

bool NDIlib_recv_ptz_is_supported(NDIlib_recv_instance_t p_instance) {
    return 0;
}

bool NDIlib_recv_ptz_zoom(NDIlib_recv_instance_t p_instance, float zoom_value) {
    return 0;
}

bool NDIlib_recv_ptz_zoom_speed(NDIlib_recv_instance_t p_instance, float zoom_speed) {
    return 0;
}

bool NDIlib_recv_ptz_pan_tilt(NDIlib_recv_instance_t p_instance, float pan_value, float tilt_value) {
    return 0;
}

bool NDIlib_recv_ptz_pan_tilt_speed(NDIlib_recv_instance_t p_instance, float pan_speed, float tilt_speed) {
    return 0;
}

bool NDIlib_recv_ptz_store_preset(NDIlib_recv_instance_t p_instance, int preset_no) {
    return 0;
}

bool NDIlib_recv_ptz_recall_preset(NDIlib_recv_instance_t p_instance, int preset_no, float speed) {
    return 0;
}

bool NDIlib_recv_ptz_auto_focus(NDIlib_recv_instance_t p_instance) {
    return 0;
}

bool NDIlib_recv_ptz_focus(NDIlib_recv_instance_t p_instance, float focus_value) {
    return 0;
}

bool NDIlib_recv_ptz_focus_speed(NDIlib_recv_instance_t p_instance, float focus_speed) {
    return 0;
}

bool NDIlib_recv_ptz_white_balance_auto(NDIlib_recv_instance_t p_instance) {
    return 0;
}

bool NDIlib_recv_ptz_white_balance_indoor(NDIlib_recv_instance_t p_instance) {
    return 0;
}

bool NDIlib_recv_ptz_white_balance_outdoor(NDIlib_recv_instance_t p_instance) {
    return 0;
}

bool NDIlib_recv_ptz_white_balance_oneshot(NDIlib_recv_instance_t p_instance) {
    return 0;
}

bool NDIlib_recv_ptz_white_balance_manual(NDIlib_recv_instance_t p_instance, float red, float blue) {
    return 0;
}

bool NDIlib_recv_ptz_exposure_auto(NDIlib_recv_instance_t p_instance) {
    return 0;
}

bool NDIlib_recv_ptz_exposure_manual(NDIlib_recv_instance_t p_instance, float exposure_level) {
    return 0;
}

bool NDIlib_recv_ptz_exposure_manual_v2(NDIlib_recv_instance_t p_instance, float iris, float gain, float shutter_speed) {
    return 0;
}

NDIlib_send_instance_t NDIlib_send_create(const NDIlib_send_create_t* p_create_settings) {
#ifdef FAKE_OBJECTS
    fake_log("sender-create"); return (NDIlib_send_instance_t)calloc(1, sizeof(struct fake_sender));
#else
    return 0;
#endif
}

void NDIlib_send_destroy(NDIlib_send_instance_t p_instance) {
#ifdef FAKE_OBJECTS
    struct fake_sender* sender = (struct fake_sender*)p_instance;
    fake_log("sender-destroy-begin");
    if (sender->completion) fake_complete(sender);
    fake_log("sender-destroy"); free(sender);
#endif
}

void NDIlib_send_send_video_v2(NDIlib_send_instance_t p_instance, const NDIlib_video_frame_v2_t* p_video_data) {
}

void NDIlib_send_send_video_async_v2(NDIlib_send_instance_t p_instance, const NDIlib_video_frame_v2_t* p_video_data) {
#ifdef FAKE_OBJECTS
    struct fake_sender* sender = (struct fake_sender*)p_instance;
    if (!p_video_data) {
        fake_log("sender-flush");
        if (sender->completion) fake_complete(sender);
        return;
    }
    fake_log("sender-submit");
    if (sender->completion) {
        const char* mode = getenv("GRAFTON_FAKE_CALLBACK_MODE");
        if (mode && !strcmp(mode, "immediate")) {
            fake_log("sender-completion");
            sender->completion(sender->opaque, p_video_data);
            /* The Rust submit call must still retain its header and pixels. */
            volatile unsigned char first_byte = *p_video_data->p_data;
            (void)first_byte;
            fake_log("sender-submit-return");
        } else {
            if (sender->count == 128) abort();
            sender->pending[sender->count++] = *p_video_data;
        }
    }
#endif
}

void NDIlib_send_send_audio_v3(NDIlib_send_instance_t p_instance, const NDIlib_audio_frame_v3_t* p_audio_data) {
}

void NDIlib_send_send_metadata(NDIlib_send_instance_t p_instance, const NDIlib_metadata_frame_t* p_metadata) {
}

bool NDIlib_send_get_tally(NDIlib_send_instance_t p_instance, NDIlib_tally_t* p_tally, uint32_t timeout_in_ms) {
    return 0;
}

int NDIlib_send_get_no_connections(NDIlib_send_instance_t p_instance, uint32_t timeout_in_ms) {
    return 0;
}

void NDIlib_send_clear_connection_metadata(NDIlib_send_instance_t p_instance) {
}

void NDIlib_send_add_connection_metadata(NDIlib_send_instance_t p_instance, const NDIlib_metadata_frame_t* p_metadata) {
}

#ifndef FAKE_OMIT_REQUIRED

void NDIlib_send_set_failover(NDIlib_send_instance_t p_instance, const NDIlib_source_t* p_failover_source) {
}
#endif

const NDIlib_source_t* NDIlib_send_get_source_name(NDIlib_send_instance_t p_instance) {
    return 0;
}

NDIlib_framesync_instance_t NDIlib_framesync_create(NDIlib_recv_instance_t p_receiver) {
    return 0;
}

void NDIlib_framesync_destroy(NDIlib_framesync_instance_t p_instance) {
}

void NDIlib_framesync_capture_audio_v2(NDIlib_framesync_instance_t p_instance, NDIlib_audio_frame_v3_t* p_audio_data, int sample_rate, int no_channels, int no_samples) {
}

void NDIlib_framesync_free_audio_v2(NDIlib_framesync_instance_t p_instance, NDIlib_audio_frame_v3_t* p_audio_data) {
}

int NDIlib_framesync_audio_queue_depth(NDIlib_framesync_instance_t p_instance) {
    return 0;
}

void NDIlib_framesync_capture_video(NDIlib_framesync_instance_t p_instance, NDIlib_video_frame_v2_t* p_video_data, NDIlib_frame_format_type_e field_type) {
}

void NDIlib_framesync_free_video(NDIlib_framesync_instance_t p_instance, NDIlib_video_frame_v2_t* p_video_data) {
}

#ifdef FAKE_OBJECTS
NDIlib_frame_type_e NDIlib_send_capture(NDIlib_send_instance_t instance, NDIlib_metadata_frame_t* metadata, uint32_t timeout) {
    metadata->length = 2;
    metadata->timecode = 123;
    metadata->p_data = (char*)malloc(2);
    metadata->p_data[0] = (char)0xff; metadata->p_data[1] = 0;
    fake_log("sender-metadata-capture");
    return NDIlib_frame_type_metadata;
}
void NDIlib_send_free_metadata(NDIlib_send_instance_t instance, const NDIlib_metadata_frame_t* metadata) {
    fake_log("sender-metadata-free"); free(metadata->p_data);
}
NDIlib_send_advertiser_instance_t NDIlib_send_advertiser_create(const NDIlib_send_advertiser_create_t* settings) {
    fake_log("send-advertiser-create"); return (NDIlib_send_advertiser_instance_t)calloc(1, 1);
}
void NDIlib_send_advertiser_destroy(NDIlib_send_advertiser_instance_t instance) {
    fake_log("send-advertiser-destroy"); free(instance);
}
bool NDIlib_send_advertiser_add_sender(NDIlib_send_advertiser_instance_t instance, NDIlib_send_instance_t sender, bool monitoring) {
    fake_log(monitoring ? "send-advertiser-add-monitoring" : "send-advertiser-add"); return true;
}
bool NDIlib_send_advertiser_del_sender(NDIlib_send_advertiser_instance_t instance, NDIlib_send_instance_t sender) {
    fake_log("send-advertiser-remove"); return true;
}
NDIlib_recv_advertiser_instance_t NDIlib_recv_advertiser_create(const NDIlib_recv_advertiser_create_t* settings) {
    fake_log("recv-advertiser-create"); return (NDIlib_recv_advertiser_instance_t)calloc(1, 1);
}
void NDIlib_recv_advertiser_destroy(NDIlib_recv_advertiser_instance_t instance) {
    fake_log("recv-advertiser-destroy"); free(instance);
}
bool NDIlib_recv_advertiser_add_receiver(NDIlib_recv_advertiser_instance_t instance, NDIlib_recv_instance_t receiver, bool controlling, bool monitoring, const char* group) {
    fake_log("recv-advertiser-add"); return true;
}
bool NDIlib_recv_advertiser_del_receiver(NDIlib_recv_advertiser_instance_t instance, NDIlib_recv_instance_t receiver) {
    fake_log("recv-advertiser-remove"); return true;
}
#endif
#ifdef FAKE_ADVANCED
NDIlib_send_instance_t NDIlib_send_create_v2(const NDIlib_send_create_t* settings, const char* json) { return NDIlib_send_create(settings); }
NDIlib_recv_instance_t NDIlib_recv_create_v4(const NDIlib_recv_create_v3_t* settings, const char* json) { return NDIlib_recv_create_v3(settings); }
void NDIlib_send_set_video_async_completion(NDIlib_send_instance_t instance, void* opaque, void (*callback)(void*, const NDIlib_video_frame_v2_t*)) {
    struct fake_sender* sender = (struct fake_sender*)instance;
    sender->opaque = opaque; sender->completion = callback;
}
#endif
