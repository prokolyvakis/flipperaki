#include "buddy_view.h"

#include <stdio.h>
#include <string.h>

#include <gui/canvas.h>

static const char* activity_text(flipperaki_buddy_Activity a) {
    switch(a) {
    case flipperaki_buddy_Activity_ACTIVITY_IDLE:
        return "Idle";
    case flipperaki_buddy_Activity_ACTIVITY_WORKING:
        return "Working";
    case flipperaki_buddy_Activity_ACTIVITY_NEEDS_ATTENTION:
        return "Needs you";
    case flipperaki_buddy_Activity_ACTIVITY_TURN_FINISHED:
        return "Done";
    case flipperaki_buddy_Activity_ACTIVITY_DISCONNECTED:
        return "Claude offline";
    default:
        return "Unknown";
    }
}

// Original 18x16 mascot: a rounded block with eyes that follow the activity.
static void draw_mascot(Canvas* canvas, int x, int y, flipperaki_buddy_Activity a) {
    canvas_draw_rframe(canvas, x, y, 18, 15, 4);
    canvas_draw_line(canvas, x + 7, y - 2, x + 9, y); // antenna
    if(a == flipperaki_buddy_Activity_ACTIVITY_IDLE ||
       a == flipperaki_buddy_Activity_ACTIVITY_DISCONNECTED) {
        canvas_draw_line(canvas, x + 4, y + 6, x + 6, y + 6);
        canvas_draw_line(canvas, x + 11, y + 6, x + 13, y + 6);
    } else {
        canvas_draw_box(canvas, x + 4, y + 4, 3, 3);
        canvas_draw_box(canvas, x + 11, y + 4, 3, 3);
    }
    if(a == flipperaki_buddy_Activity_ACTIVITY_NEEDS_ATTENTION) {
        canvas_draw_circle(canvas, x + 9, y + 11, 1);
    } else {
        canvas_draw_line(canvas, x + 6, y + 10, x + 11, y + 10);
    }
}

static const char* basename_of(const char* path) {
    const char* slash = strrchr(path, '/');
    return slash ? slash + 1 : path;
}

static void draw_prompt(Canvas* canvas, const BuddyViewModel* m) {
    const flipperaki_buddy_ArmRequest* r = &m->core.prompt;
    char line[104]; // fits the longest app_path; the screen clips
    canvas_set_font(canvas, FontPrimary);
    canvas_draw_str(canvas, 2, 10, "Allow control?");
    canvas_set_font(canvas, FontSecondary);
    snprintf(line, sizeof(line), "App: %s", r->app_label);
    canvas_draw_str(canvas, 2, 22, line);
    snprintf(line, sizeof(line), "(%s)", basename_of(r->app_path));
    canvas_draw_str(canvas, 2, 31, line);
    snprintf(line, sizeof(line), "By: %s", r->client_label);
    canvas_draw_str(canvas, 2, 40, line);
    snprintf(
        line,
        sizeof(line),
        "%lu actions, %lus left",
        (unsigned long)r->max_actions,
        (unsigned long)buddy_core_prompt_seconds_left(&m->core, m->now_ms));
    canvas_draw_str(canvas, 2, 49, line);
    canvas_draw_str(
        canvas, 2, 62, m->core.armed ? "Hold OK: allow  Back: deny" : "Release all buttons");
}

static void draw_status(Canvas* canvas, const BuddyViewModel* m) {
    const BuddyCore* c = &m->core;
    char line[56];
    flipperaki_buddy_Activity a = c->has_snapshot ? c->snapshot.activity :
                                                    flipperaki_buddy_Activity_ACTIVITY_IDLE;
    draw_mascot(canvas, 2, 4, a);
    canvas_set_font(canvas, FontPrimary);
    canvas_draw_str(canvas, 26, 12, c->link == BuddyLinkReady ? activity_text(a) : "Flipperaki");
    canvas_set_font(canvas, FontSecondary);

    if(c->link != BuddyLinkReady) {
        canvas_draw_str(canvas, 2, 34, "Waiting for the host...");
    } else if(c->has_snapshot) {
        snprintf(
            line,
            sizeof(line),
            "%s  %lu session(s)",
            c->snapshot.session_label,
            (unsigned long)c->snapshot.active_sessions);
        canvas_draw_str(canvas, 2, 30, line);
        canvas_draw_str(canvas, 2, 40, c->snapshot.detail);
    }

    const char* verdict = c->outcome_verdict == flipperaki_buddy_Verdict_VERDICT_APPROVE ?
                              "Allowed" :
                              "Denied";
    switch(c->outcome) {
    case BuddyOutcomeSent:
        snprintf(line, sizeof(line), "%s #%lu, unconfirmed", verdict, (unsigned long)c->outcome_request_id);
        break;
    case BuddyOutcomeAccepted:
        snprintf(line, sizeof(line), "%s #%lu, confirmed", verdict, (unsigned long)c->outcome_request_id);
        break;
    case BuddyOutcomeRejected:
        snprintf(line, sizeof(line), "%s #%lu, host refused", verdict, (unsigned long)c->outcome_request_id);
        break;
    default:
        line[0] = '\0';
        break;
    }
    canvas_draw_str(canvas, 2, 51, line);
    canvas_draw_str(canvas, 2, 62, "Hold Back: quit");
}

static void draw_idle(Canvas* canvas) {
    draw_mascot(canvas, 2, 4, flipperaki_buddy_Activity_ACTIVITY_IDLE);
    canvas_set_font(canvas, FontPrimary);
    canvas_draw_str(canvas, 26, 12, "Flipperaki");
    canvas_set_font(canvas, FontSecondary);
    canvas_draw_str(canvas, 2, 30, "Start Flipperaki on your");
    canvas_draw_str(canvas, 2, 40, "computer; it opens this");
    canvas_draw_str(canvas, 2, 50, "app over USB or Bluetooth.");
    canvas_draw_str(canvas, 2, 62, "Back: exit");
}

static void buddy_view_draw(Canvas* canvas, void* model) {
    const BuddyViewModel* m = model;
    canvas_clear(canvas);
    if(!m->rpc_mode) {
        draw_idle(canvas);
    } else if(m->core.prompt_active) {
        draw_prompt(canvas, m);
    } else {
        draw_status(canvas, m);
    }
}

// Consume every ASCII event. Unconsumed ASCII makes ViewDispatcher synthesize
// InputEvents with sequence_source left 0 (= HARDWARE) (view_dispatcher.c,
// view_dispatcher_handle_ascii), which would otherwise look physical.
static bool buddy_view_ascii(AsciiEvent* event, void* context) {
    UNUSED(event);
    UNUSED(context);
    return true;
}

View* buddy_view_alloc(void) {
    View* view = view_alloc();
    view_allocate_model(view, ViewModelTypeLocking, sizeof(BuddyViewModel));
    view_set_draw_callback(view, buddy_view_draw);
    view_set_ascii_callback(view, buddy_view_ascii);
    return view;
}

void buddy_view_free(View* view) {
    view_free(view);
}

void buddy_view_update(View* view, const BuddyCore* core, uint32_t now_ms, bool rpc_mode) {
    with_view_model(
        view,
        BuddyViewModel * m,
        {
            m->core = *core;
            m->now_ms = now_ms;
            m->rpc_mode = rpc_mode;
        },
        true);
}
