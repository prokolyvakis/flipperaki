#pragma once
// The single Buddy screen. Drawing reads only the view model snapshot.

#include <gui/view.h>

#include "buddy_core.h"

typedef struct {
    BuddyCore core; // copy taken on the app thread
    uint32_t now_ms;
    bool rpc_mode;
} BuddyViewModel;

// Allocates the view with draw and ASCII callbacks; the caller sets input + context.
View* buddy_view_alloc(void);
void buddy_view_free(View* view);
void buddy_view_update(View* view, const BuddyCore* core, uint32_t now_ms, bool rpc_mode);
