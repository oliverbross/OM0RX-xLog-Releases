#pragma once
#include <stdint.h>

int xlog_private_windows_path(const uint16_t *path, int directory);
int xlog_create_private_windows_directory(const uint16_t *path);
