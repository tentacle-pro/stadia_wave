# macOS上开发esp32环境搭建
> 来自 https://docs.espressif.com/projects/esp-idf/zh_CN/stable/esp32s3/get-started/index.html#get-started-build
- 所需的软件
brew install libgcrypt glib pixman sdl2 libslirp dfu-util cmake python
brew tap espressif/eim
brew install eim
eim install


- VSCode ESP-IDF插件

- 命令行编译要先激活环境
cd esp32
sh prepare_runtime.sh

- ESP32被识别为USB设备
ll /dev/cu.*

- 具体开发过程见官方入门文档
> https://docs.espressif.com/projects/esp-idf/zh_CN/stable/esp32s3/get-started/linux-macos-start-project.html

