/* Diagnostic only. Repeats the NDI registration sequence without the Rust API. */
#ifndef _WIN32
#define _POSIX_C_SOURCE 200809L
#endif
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <windows.h>
#else
#include <time.h>
#include <unistd.h>
#endif
#include <Processing.NDI.Lib.h>

static uint64_t now_ms(void) {
#ifdef _WIN32
    return GetTickCount64();
#else
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
#endif
}

static unsigned long process_id(void) {
#ifdef _WIN32
    return GetCurrentProcessId();
#else
    return (unsigned long)getpid();
#endif
}

static bool loopback_endpoint(NDIlib_send_instance_t sender, char *out, size_t size) {
    const NDIlib_source_t *sent = NDIlib_send_get_source_name(sender);
    if (!sent || !sent->p_ndi_name) return false;
    NDIlib_find_create_t options = {0};
    options.show_local_sources = true;
    options.p_extra_ips = "127.0.0.1";
    NDIlib_find_instance_t finder = NDIlib_find_create_v2(&options);
    if (!finder) return false;
    bool found = false;
    const uint64_t end = now_ms() + 15000;
    while (!found && now_ms() < end) {
        uint32_t count = 0;
        const NDIlib_source_t *sources = NDIlib_find_get_current_sources(finder, &count);
        for (uint32_t index = 0; index < count; ++index) {
            const NDIlib_source_t *source = &sources[index];
            if (!source->p_ndi_name || !source->p_url_address
                || strcmp(source->p_ndi_name, sent->p_ndi_name)) continue;
            if (strstr(source->p_url_address, "://")) continue;
            const char *port = strrchr(source->p_url_address, ':');
            if (!port) continue;
            int length = snprintf(out, size, "127.0.0.1%s", port);
            found = length > 0 && (size_t)length < size;
            if (found) break;
        }
        if (!found) NDIlib_find_wait_for_sources(finder, 100);
    }
    NDIlib_find_destroy(finder);
    return found;
}

static bool receive_metadata(NDIlib_send_instance_t sender, const char *text, uint32_t timeout) {
    NDIlib_metadata_frame_t metadata = {0};
    if (NDIlib_send_capture(sender, &metadata, timeout) != NDIlib_frame_type_metadata) return false;
    bool matches = metadata.p_data && !strcmp(metadata.p_data, text);
    NDIlib_send_free_metadata(sender, &metadata);
    return matches;
}

static bool expect_registration(NDIlib_send_instance_t sender, NDIlib_recv_instance_t receiver,
                                NDIlib_video_frame_v2_t *video, const char *text, uint64_t budget,
                                unsigned *pictures) {
    const uint64_t end = now_ms() + budget;
    *pictures = 0;
    while (now_ms() < end) {
        NDIlib_send_send_video_v2(sender, video);
        NDIlib_video_frame_v2_t captured = {0};
        if (NDIlib_recv_capture_v3(receiver, &captured, NULL, NULL, 10)
            == NDIlib_frame_type_video) {
            NDIlib_recv_free_video_v2(receiver, &captured);
            ++*pictures;
        }
        if (receive_metadata(sender, text, 100)) return true;
    }
    return false;
}

enum trial_result { setup_error = -1, success, initial_miss, reconnect_miss, disconnect_miss };

static enum trial_result trial(const char *mode, unsigned round, unsigned reconnects) {
    const bool by_address = !strncmp(mode, "address", 7);
    const bool observe_disconnect = strstr(mode, "observed") != NULL;
    char name[100];
    char endpoint[100];
    snprintf(name, sizeof name, "Raw NDI metadata %lu %u", process_id(), round);
    NDIlib_send_create_t send_options = {0};
    send_options.p_ndi_name = name;
    send_options.clock_video = false;
    send_options.clock_audio = true;
    NDIlib_send_instance_t sender = NDIlib_send_create(&send_options);
    if (!sender) return setup_error;
    NDIlib_recv_create_v3_t recv_options = {0};
    recv_options.source_to_connect_to.p_ndi_name = "";
    recv_options.color_format = NDIlib_recv_color_format_BGRX_BGRA;
    recv_options.bandwidth = NDIlib_recv_bandwidth_highest;
    recv_options.allow_video_fields = true;
    NDIlib_recv_instance_t receiver = NDIlib_recv_create_v3(&recv_options);
    if (!receiver) {
        NDIlib_send_destroy(sender);
        return setup_error;
    }
    NDIlib_metadata_frame_t registered = {0};
    char original[] = "<receiver-registration/>";
    registered.p_data = original;
    registered.length = (int)sizeof original;
    registered.timecode = 21;
    NDIlib_recv_add_connection_metadata(receiver, &registered);
    NDIlib_source_t target = {0};
    enum trial_result outcome = setup_error;
    if (loopback_endpoint(sender, endpoint, sizeof endpoint)) {
        target.p_ndi_name = by_address ? "" : NDIlib_send_get_source_name(sender)->p_ndi_name;
        target.p_url_address = endpoint;
        uint8_t pixels[16 * 16 * 4 + 64] = {0};
        NDIlib_video_frame_v2_t video = {0};
        video.xres = 16;
        video.yres = 16;
        video.FourCC = NDIlib_FourCC_video_type_BGRA;
        video.frame_rate_N = 60;
        video.frame_rate_D = 1;
        video.picture_aspect_ratio = 1.0f;
        video.frame_format_type = NDIlib_frame_format_type_progressive;
        video.timecode = NDIlib_send_timecode_synthesize;
        video.p_data = pixels;
        video.line_stride_in_bytes = 16 * 4;
        outcome = success;
        for (unsigned stage = 0; stage <= reconnects; ++stage) {
            uint64_t to_zero = 0;
            int disconnected_sender = -1, disconnected_receiver = -1;
            if (stage > 0) {
                for (;;) {
                    NDIlib_metadata_frame_t metadata = {0};
                    if (NDIlib_send_capture(sender, &metadata, 0) != NDIlib_frame_type_metadata) break;
                    NDIlib_send_free_metadata(sender, &metadata);
                }
                NDIlib_recv_connect(receiver, NULL);
                const uint64_t disconnected = now_ms();
                if (observe_disconnect) {
                    for (;;) {
                        disconnected_sender = NDIlib_send_get_no_connections(sender, 0);
                        disconnected_receiver = NDIlib_recv_get_no_connections(receiver);
                        to_zero = now_ms() - disconnected;
                        if (!disconnected_sender && !disconnected_receiver) break;
                        if (to_zero >= 5000) {
                            outcome = disconnect_miss;
                            break;
                        }
#ifdef _WIN32
                        Sleep(5);
#else
                        struct timespec pause = {0, 5000000};
                        nanosleep(&pause, NULL);
#endif
                    }
                    printf("raw disconnected mode=%s round=%u stage=%u sender=%d receiver=%d to_zero=%" PRIu64 "ms\n",
                           mode, round, stage, disconnected_sender, disconnected_receiver, to_zero);
                    fflush(stdout);
                    if (outcome == disconnect_miss) break;
                }
            }
            const uint64_t started = now_ms();
            NDIlib_recv_connect(receiver, &target);
            unsigned pictures = 0;
            bool seen = expect_registration(sender, receiver, &video, original, 10000, &pictures);
            printf("raw mode=%s round=%u stage=%u port=%s received=%d video=%u elapsed=%" PRIu64
                   "ms sender=%d receiver=%d\n",
                   mode, round, stage, endpoint, seen, pictures, now_ms() - started,
                   NDIlib_send_get_no_connections(sender, 0),
                   NDIlib_recv_get_no_connections(receiver));
            fflush(stdout);
            if (seen) continue;
            char live_xml[] = "<manual-live/>";
            char add_xml[] = "<registered-live/>";
            NDIlib_metadata_frame_t live = {0};
            NDIlib_metadata_frame_t added = {0};
            live.p_data = live_xml;
            live.length = (int)sizeof live_xml;
            added.p_data = add_xml;
            added.length = (int)sizeof add_xml;
            bool accepted = NDIlib_recv_send_metadata(receiver, &live);
            bool delivered = expect_registration(sender, receiver, &video, live_xml, 2200, &pictures);
            NDIlib_recv_add_connection_metadata(receiver, &added);
            bool delivered_add = expect_registration(sender, receiver, &video, add_xml, 2200, &pictures);
            printf("raw lost stage=%u direct_accepted=%d direct_delivered=%d add_while_connected_delivered=%d\n",
                   stage, accepted, delivered, delivered_add);
            outcome = stage ? reconnect_miss : initial_miss;
            break;
        }
    }
    NDIlib_recv_clear_connection_metadata(receiver);
    NDIlib_recv_destroy(receiver);
    NDIlib_send_destroy(sender);
    return outcome;
}

#ifdef _WIN32
static bool runtime_matches(void) {
    wchar_t actual[4096], expected[4096];
    HMODULE dll = GetModuleHandleW(L"Processing.NDI.Lib.x64.dll");
    const DWORD actual_len = dll ? GetModuleFileNameW(dll, actual, 4096) : 0;
    const DWORD expected_len = GetEnvironmentVariableW(L"GRAFTON_NDI_EXPECTED_RUNTIME", expected, 4096);
    if (!actual_len || actual_len >= 4096 || !expected_len || expected_len >= 4096) return false;
    for (DWORD index = 0; index < expected_len; ++index) {
        if (expected[index] == L'/') expected[index] = L'\\';
    }
    printf("Raw SDK DLL: %ls\n", actual);
    return _wcsicmp(actual, expected) == 0;
}
#endif

int main(int argc, char **argv) {
    if (argc != 4 || !NDIlib_initialize()) return 2;
#ifdef _WIN32
    if (!runtime_matches()) {
        fputs("Unexpected native SDK DLL\n", stderr);
        NDIlib_destroy();
        return 4;
    }
#endif
    const char *mode = argv[1];
    unsigned count = (unsigned)strtoul(argv[2], NULL, 10);
    unsigned reconnects = (unsigned)strtoul(argv[3], NULL, 10);
    printf("Raw SDK runtime: %s\n", NDIlib_version());
    unsigned initial = 0, later = 0, disconnect = 0, completed = 0, reconnects_checked = 0;
    fflush(stdout);
    for (unsigned index = 0; index < count && initial + later + disconnect < 4; ++index) {
        enum trial_result result = trial(mode, index, reconnects);
        if (result == setup_error) {
            fputs("Could not establish the diagnostic sender/receiver pair\n", stderr);
            NDIlib_destroy();
            return 3;
        }
        ++completed;
        initial += result == initial_miss;
        later += result == reconnect_miss;
        disconnect += result == disconnect_miss;
        if (result == success) reconnects_checked += reconnects;
    }
    printf("raw summary mode=%s trials=%u initial_misses=%u reconnect_misses=%u disconnect_misses=%u reconnects_in_passing_trials=%u\n",
           mode, completed, initial, later, disconnect, reconnects_checked);
    NDIlib_destroy();
}
