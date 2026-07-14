#pragma once

#include <QString>

class QQmlApplicationEngine;

namespace llmux_islands {

void initialize_application();
void prepare_surface(const QString &mode);
void load_application(QQmlApplicationEngine &engine);
bool configure_surface(QQmlApplicationEngine &engine, const QString &mode);
int exec_application();

} // namespace llmux_islands
