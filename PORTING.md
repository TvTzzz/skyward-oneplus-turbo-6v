# Skyward 移植记录：OnePlus Turbo 6V（OP64D3L1）

把 [Skyward](https://github.com/kirklin/skyward) 移植到一加 Turbo 6V / PLY110 的全过程记录。
包含每个坑的现象、日志、原因和修法，以及新增功能的说明。

---

## 1. 设备信息

| 项 | 值 |
|---|---|
| 型号 | OnePlus Turbo 6V（PLY110） |
| 代号 | `OP64D3L1` |
| 平台 | Qualcomm SM7635（`volcano`） |
| 系统 | Android 16（SDK 36），ColorOS |
| 分区 | A/B + Virtual A/B，动态分区 |
| 触摸 | OPLUS HBP 层（`aw/aw_haptic`、`oplus_hbp_core`、`/dev/hbp0`、`/dev/tcm0`） |
| 振动 | AW8697，走 timed-output LED 接口（`/sys/class/leds/vibrator`），`f0 = 1728`（172.8 Hz） |
| 电量计 | `bq27541`（I2C `13-0055`），但充电框架初始化竞态导致 recovery 里不可用 |

---

## 2. 构建

工具链需要在工作区内（沙盒环境下 `~/.cargo`、`~/.rustup` 只读）：

```sh
export RUSTUP_HOME="$PWD/.toolchain/rustup"
export CARGO_HOME="$PWD/.toolchain/cargo"
export PATH="$CARGO_HOME/bin:$PATH"
export ANDROID_NDK_HOME=~/android/android-ndk-r29
```

三个产物：

```sh
# 1. 主体（动态链接）
cargo ndk -t arm64-v8a --platform 30 build --release -p skyward

# 2. system-init（必须静态链接，单独 target-dir）
RUSTFLAGS="-C target-feature=+crt-static" \
  cargo ndk -t arm64-v8a --platform 30 build --release -p system-init \
  --target-dir target/system-init

# 3. 打包器（宿主机）
cargo build --release -p skyward-pack
```

> **注意**：改了 `skyward-config` 的结构体后必须重编 `skyward-pack`，否则打包时报
> `parse device config … missing field`。

打包：

```sh
./target/release/skyward-pack recovery \
  --stock stock/OP64D3L1/recovery.img \
  --binary target/aarch64-linux-android/release/skyward \
  --system-init target/system-init/aarch64-linux-android/release/system-init \
  --config devices/OP64D3L1.toml \
  --font resources/fonts/MiSansVF.ttf \
  --mono resources/fonts/KirkMono-Regular.ttf \
  --mono-license resources/fonts/KirkMono-OFL.txt \
  --out out/skyward-OP64D3L1.img
```

刷入：

```sh
adb push out/skyward-OP64D3L1.img /tmp/
adb shell dd if=/tmp/skyward-OP64D3L1.img of=/dev/block/by-name/recovery_a
adb reboot recovery
```

---

## 3. 踩过的坑

### 3.1 黑屏：找不到亮度最大值

```
panel … gives its normal maximum as none of [("mi,dsi-dsi0-…")]
```

一加的面板用的是 `oplus,dsi-brightness-normal-max-level`，不是上游假设的小米那套。

**修法**：在 `skyward/src/ui.rs` 的 `NORMAL_MAX` 里加上这一对。

### 3.2 触摸不工作：OPLUS 私有 HBP 层

这不是普通 evdev 触摸。内核通过 `oplus_hbp_core` 把触摸数据转发给 HAL：

```
/dev/hbp0               触摸数据
/dev/tcm0               控制通道
```

**修法**：启动 `vendor-oplus-hardware-touch-V2-hbp5-service`，并且：

- `groups = ["root", "system"]`（默认组里找 `inet` 会失败）
- 必须在 gatekeeper **之前**启动，否则等 HBP 超时

### 3.3 gatekeeper 反复崩溃

```
cannot execv … libqtigatekeeper.so: No such file or directory
```

库在 `/vendor/lib64/hw`，要把这个目录加进库搜索路径。

接着又报：

```
Could not find … in the VINTF manifest
```

**修法**：`description = "/vendor/etc/vintf/manifest_volcano.xml"`。

gatekeeper 初始化很慢（约 20 秒），所以放在服务列表**最后**，避免拖慢其他服务。

### 3.4 进入 recovery 后 adb / MTP 消失

```
sys.usb.state is Some("none") after 5000 ms, not "mtp,adb"
```

**修法**（两处）：

1. `skyward-sys/src/usb.rs`：设置配置时先写 `sys.usb.config_disable=0`
2. 切换失败时回退到 `adb`

### 3.5 APEX 挂载 EIO

```
avc: denied { use } … scontext=u:r:kernel:s0 tcontext=u:r:recovery:s0 tclass=fd
```

OPPO 的 recovery sepolicy 少了 `allow kernel recovery:fd use`，导致 loop 设备读不了。

**修法**：`skyward/src/secure.rs` 里 `bundle()` 失败时用 permissive 重试
（写 0 到 `/sys/fs/selinux/enforce`）。

### 3.6 进入 recovery 300 秒后自动重启

```
vendor.oplus.phoenix.recovery.cmd = reboot_300_0
```

OPPO 的 `phoenix_recovery` 是个倒计时炸弹。

**修法**：启动时 `ctl.stop phoenix_recovery`。

### 3.7 hwservicemanager 崩溃循环

```
cannot execv('/system/bin/hwservicemanager') … No such file or directory
```

它依赖 `/system/bin/bootstrap/linker64`，recovery 里没有。

**修法**：直接 `ctl.stop hwservicemanager`。

### 3.8 WLAN 起不来

```
cfg80211: Unknown symbol rfkill_alloc (err -2)
finit_module … ENOENT
```

**修法**：

1. 模块列表必须 **`rfkill` 打头**
2. 拉起方式是写 `1` 到 `/sys/kernel/icnss/wpss_boot`
3. 从写到 `wlan0` 出现需要 **30–40 秒**（驱动向 modem 的 DMS 服务要 MAC，
   重试十几秒才放弃），所以 `INTERFACE_WAIT` 从 20 秒改到 75 秒
4. 固件在 `bring_up` 阶段就预启动，让这几十秒发生在用户打开 WLAN 页面之前

### 3.9 从 fastbootd 返回后没有触摸

```
devices not brought up: android.hardware.health.IHealth/default has not registered again…
```

健康检查的超时中断了 `bring_up`，导致后面的 `services::start` 没执行。

**修法**：`publish_descriptions` 改为非致命（`not every service came back to the directory`）。

### 3.10 从 fastbootd 返回后没有振动

```
aw8697_haptic_170.bin: load failed with -13
```

驱动在开机第 9 秒就去 persist 找波形固件，那时分区还没挂上，失败后就一直拒绝播放。

**修法**：配置里加

```toml
reload = "/sys/class/leds/vibrator/ram_update"
```

在打开和从 fastbootd 返回后各写一次 `1`。

### 3.11 fastboot 页面黑屏 5 秒

`usb::switch` 是阻塞调用。**修法**：挪到独立线程。

### 3.12 熄屏后再也点不亮

熄屏 5 秒后 `turn_off()` 把 CRTC 的 `ACTIVE` 置 false，OPPO 的 `msm_drm` 驱动
**无法从完全关闭状态恢复**——电源键按下后界面不回来。

**修法**：让面板一直停在低功耗待机（LP2），不进入 `turn_off`：

```rust
const REST: Duration = Duration::from_secs(24 * 60 * 60);
```

代价是待机功耗略高，换来的是瞬间唤醒。

### 3.13 启动画面很暗

开机时背光停在 bootloader 留下的值（`200/4094`，约 5%），Skyward 要到 UI 循环
里才调背光。

**修法**：`splash()` 里先读设置、写背光，再画第一帧。

### 3.14 启动画面和界面之间还有黑屏

`drop(surface)` 释放 DRM master 时面板会灭，`ui::run` 再打开一次就有空档。

**修法**：`splash()` 返回 `KmsSurface`，`ui::run` 直接接管，不重新打开。

### 3.15 启动时 adb 断连

```
init: Sending signal 9 to service 'adbd'
android_work: sent uevent USB_STATE=DISCONNECTED
init: Processed ctl.restart for 'adbd' from pid … (skyward)
```

无线调试需要在 adbd **启动前**设置 `service.adb.tcp.port`，否则只能重启 adbd，
而重启会断 USB。

**修法**：在 `recovery()` 开头读设置并设置属性，adbd 起来时端口就已经对了。

### 3.16 振动强度调了没用

`brightness` 被驱动完全忽略——播放的是 RAM 里的波形，振幅只认 `gain`：

```
/sys/class/leds/vibrator/gain     0x00 – 0xFF
```

而且驱动**播放完会把 gain 复位成 0x80**，所以每次振动前都要重写。

**修法**：配置加 `gain = "/sys/class/leds/vibrator/gain"`，
滑条位置映射到 `gain = 0x33 – 0xD9`。

### 3.17 电量显示飘

recovery 里 `/sys/class/power_supply/` 是**空的**。原因是内核充电框架的初始化竞态：

```
0.916s  MMS_GAUGE: gauge_ic[oplus,virtual_gauge] 0 init timeout
1.000s  BQ27541: register bq27541-battery
```

充电框架比电量计先起来 84ms，等不到就超时，之后所有电量接口都锁死为 0。
模块 `refcnt=5` 卸不掉，`CONFIG_MODULE_FORCE_UNLOAD` 也没开，userspace 无法恢复。

**修法**：退回 PMIC 的 VADC，做三件事：

1. 电压查表换算电量
2. 用输入电流（`in_current_…iin_fb_input`）判断是否充电，充电时减掉 40mV 抬升
3. **低通滤波**（时间常数 50 秒）+ **单调约束**（充电时只升、放电时只降）+
   插入检测**迟滞**（>150mA 认为插上，<50mA 认为拔掉）

---

## 4. 新增功能

除了上游已有的功能，这个移植额外加了：

| 功能 | 说明 |
|---|---|
| 刷新率切换 | 面板提供 30/60/90/120/144 Hz，设置页可选 |
| 12 小时制 | 时钟显示 AM/PM |
| 无线调试 | adbd 监听 5555，页面显示 `adb connect <IP>:5555` |
| 自动解密 | 凭据存 `/metadata/skyward/credential`，拨开关时弹密码验证 |
| WLAN 记忆 | 记住 SSID + 密码，可在「已保存的网络」页查看明文、删除、开关自动连接 |
| 振动 | LED 接口 + `gain` 调强度，点击、打字、图案解锁都震 |
| 启动画面 | 挂载分区前先画一帧，避免黑屏 |
| 电量估算 | VADC + 电流补偿 + 滤波 + 单调约束 |
| 忽略电量检测 | 设置里可关掉安装前的电量门槛 |
| 关闭启动动画 | 设置里可跳过启动画面 |
| 移交裸显示 | `persist.skyward.run` 属性可让 Skyward 在拿 DRM 前执行脚本 |

---

## 5. 已知限制

### 5.1 电量不是真值

见 3.17。数字够用来判断"能不能刷机"，但不是精确电量。系统自己的
`capacity` 和 `charge_counter/charge_full` 都能差 7 个点。

### 5.2 只能在 recovery 里用

Skyward 安装的是 recovery 分区，不碰系统。

---

## 6. 验证过的功能

- [x] 触摸、显示、亮度、刷新率
- [x] 解密（PIN / 密码 / 图案）
- [x] 自动解密
- [x] WLAN 连接、记忆、自动连接
- [x] 无线调试
- [x] MTP
- [x] 振动（点击 / 打字 / 图案 / 强度调节）
- [x] 从 fastbootd 返回
- [x] 备份（未完整验证恢复写入）
- [ ] zip 安装 / OTA 刷写
- [ ] 清除数据

---

## 7. 文件

- `OP64D3L1.toml` —— 设备配置，放到 `devices/` 下
- `skyward-OP64D3L1.patch` —— 相对上游 `main` 的完整改动（16 个文件）

应用补丁：

```sh
git clone https://github.com/kirklin/skyward
cd skyward
git apply /path/to/skyward-OP64D3L1.patch
```

---

## 8. 致谢

- [kirklin/skyward](https://github.com/kirklin/skyward) —— 上游项目
- 代码中大量注释沿用了上游的写作风格（用英文写"做了什么、为什么"），
  新增部分保持一致
