#include "flipperaki_app.h"

#include <stdio.h>

#include <furi.h>
#include <furi_hal_gpio.h>
#include <furi_hal_resources.h>
#include <gui/gui.h>
#include <gui/view_dispatcher.h>

#include "buddy_core.h"
#include "buddy_protocol.h"
#include "buddy_rpc.h"
#include "buddy_view.h"

#define VIEW_ID 0
#define TICK_PERIOD_MS 500

struct FlipperakiApp {
    Gui* gui;
    ViewDispatcher* view_dispatcher;
    View* view;
    BuddyRpc* rpc; // NULL when not launched by the host
    BuddyCore core; // written only on the event loop thread
    // Large buffers live here, not on the app stack.
    BuddyIngress ingress;
    BuddyActions actions;
    uint8_t tx[BUDDY_MESSAGE_MAX_BYTES];
};

// The core counts milliseconds; furi ticks are milliseconds (checked at alloc).
static uint32_t now_ms(void) {
    return furi_get_tick();
}

static void refresh(FlipperakiApp* app) {
    buddy_view_update(app->view, &app->core, now_ms(), app->rpc != NULL);
}

static void apply(FlipperakiApp* app) {
    for(uint8_t i = 0; i < app->actions.send_count; i++) {
        size_t size = 0;
        // Every Envelope fits: generated max size is checked <= 512 at build time.
        furi_check(buddy_protocol_encode(&app->actions.send[i], app->tx, sizeof(app->tx), &size));
        buddy_rpc_send(app->rpc, app->tx, size);
    }
    if(app->actions.exit) {
        buddy_rpc_close(app->rpc);
        view_dispatcher_stop(app->view_dispatcher);
    }
    refresh(app);
}

static void on_ingress(FuriEventLoopObject* object, void* context) {
    UNUSED(object);
    FlipperakiApp* app = context;
    if(!buddy_rpc_receive(app->rpc, &app->ingress)) return;
    if(buddy_rpc_is_closing(app->rpc)) {
        buddy_core_on_session_close(&app->core);
        refresh(app);
        return;
    }
    buddy_core_on_message(
        &app->core, now_ms(), app->ingress.bytes, app->ingress.size, &app->actions);
    apply(app);
}

static void on_tick(void* context) {
    FlipperakiApp* app = context;
    if(app->rpc != NULL && buddy_rpc_is_closing(app->rpc)) {
        buddy_core_on_session_close(&app->core);
    }
    buddy_core_on_tick(&app->core, now_ms());
    refresh(app);
}

static bool map_input(const InputEvent* event, BuddyKey* key, BuddyInputType* type) {
    switch(event->key) {
    case InputKeyUp: *key = BuddyKeyUp; break;
    case InputKeyDown: *key = BuddyKeyDown; break;
    case InputKeyRight: *key = BuddyKeyRight; break;
    case InputKeyLeft: *key = BuddyKeyLeft; break;
    case InputKeyOk: *key = BuddyKeyOk; break;
    case InputKeyBack: *key = BuddyKeyBack; break;
    default: return false;
    }
    switch(event->type) {
    case InputTypePress: *type = BuddyInputPress; break;
    case InputTypeRelease: *type = BuddyInputRelease; break;
    case InputTypeShort: *type = BuddyInputShort; break;
    case InputTypeLong: *type = BuddyInputLong; break;
    case InputTypeRepeat: *type = BuddyInputRepeat; break;
    default: return false;
    }
    return true;
}

// Samples the button's GPIO the same way the input service does (pressed when
// the level differs from `inverted`). Injected events cannot move a pin.
static bool key_physically_down(InputKey key) {
    for(size_t i = 0; i < input_pins_count; i++) {
        if(input_pins[i].key == key) {
            return furi_hal_gpio_read(input_pins[i].gpio) ^ input_pins[i].inverted;
        }
    }
    return false;
}

// Consumes every event: in RPC mode Back must never navigate away on its own.
static bool on_input(InputEvent* event, void* context) {
    FlipperakiApp* app = context;
    if(app->rpc == NULL) {
        if(event->key == InputKeyBack && event->type == InputTypeShort) {
            view_dispatcher_stop(app->view_dispatcher);
        }
        return true;
    }
    BuddyKey key;
    BuddyInputType type;
    if(!map_input(event, &key, &type)) return true;
    // RPC GUI input is marked SOFTWARE, but the CLI's `input send` publishes events
    // without setting sequence_source (0 == HARDWARE), so the marker alone is not
    // proof. A press or long press also requires the real button pin to be down.
    bool hardware = event->sequence_source == INPUT_SEQUENCE_SOURCE_HARDWARE;
    if(hardware && (event->type == InputTypePress || event->type == InputTypeLong)) {
        hardware = key_physically_down(event->key);
    }
    BuddyOrigin origin = hardware ? BuddyOriginHardware : BuddyOriginSoftware;
    if(buddy_rpc_is_closing(app->rpc)) buddy_core_on_session_close(&app->core);
    buddy_core_on_input(&app->core, now_ms(), origin, key, type, &app->actions);
    apply(app);
    return true;
}

FlipperakiApp* flipperaki_app_alloc(const char* args) {
    furi_check(furi_kernel_get_tick_frequency() == 1000);

    FlipperakiApp* app = malloc(sizeof(FlipperakiApp));
    memset(app, 0, sizeof(*app));
    buddy_core_init(&app->core);

    app->gui = furi_record_open(RECORD_GUI);
    app->view_dispatcher = view_dispatcher_alloc();
    app->view = buddy_view_alloc();
    view_set_context(app->view, app);
    view_set_input_callback(app->view, on_input);
    view_dispatcher_add_view(app->view_dispatcher, VIEW_ID, app->view);
    view_dispatcher_set_event_callback_context(app->view_dispatcher, app);
    view_dispatcher_set_tick_event_callback(
        app->view_dispatcher, on_tick, furi_ms_to_ticks(TICK_PERIOD_MS));
    view_dispatcher_attach_to_gui(app->view_dispatcher, app->gui, ViewDispatcherTypeFullscreen);
    view_dispatcher_switch_to_view(app->view_dispatcher, VIEW_ID);

    void* rpc_ctx = NULL;
    if(args != NULL && sscanf(args, "RPC %p", &rpc_ctx) == 1 && rpc_ctx != NULL) {
        app->rpc = buddy_rpc_alloc(
            rpc_ctx, view_dispatcher_get_event_loop(app->view_dispatcher), on_ingress, app);
    }
    refresh(app);
    return app;
}

void flipperaki_app_run(FlipperakiApp* app) {
    view_dispatcher_run(app->view_dispatcher);
}

void flipperaki_app_free(FlipperakiApp* app) {
    if(app->rpc != NULL) buddy_rpc_free(app->rpc);
    view_dispatcher_remove_view(app->view_dispatcher, VIEW_ID);
    buddy_view_free(app->view);
    view_dispatcher_free(app->view_dispatcher);
    furi_record_close(RECORD_GUI);
    free(app);
}

int32_t flipperaki_app(void* p) {
    FlipperakiApp* app = flipperaki_app_alloc(p);
    flipperaki_app_run(app);
    flipperaki_app_free(app);
    return 0;
}
