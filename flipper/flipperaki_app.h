#pragma once

#include <stdint.h>

typedef struct FlipperakiApp FlipperakiApp;

// args: "RPC <ptr>" when launched by the host through RPC AppStart; anything else
// shows the idle screen.
FlipperakiApp* flipperaki_app_alloc(const char* args);
void flipperaki_app_run(FlipperakiApp* app);
void flipperaki_app_free(FlipperakiApp* app);

int32_t flipperaki_app(void* p);
