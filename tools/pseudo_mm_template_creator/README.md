## pseudo_mm_template_creator 使用说明

### 编译方式

- 推荐在 Firecracker 提供的 devtool 环境中编译，确保使用正确的 musl 工具链：
  ```bash
  cd /root/faasnap-firecracker
  tools/devtool build -- --package pseudo_mm_template_creator --bin pseudo_mm_template_creator
  ```
- 编译产物位于 `build/cargo_target/x86_64-unknown-linux-musl/debug/pseudo_mm_template_creator`，可根据需要自行复制或 strip。

### 基本用法

- 单次生成模板：
  ```bash
  ./build/cargo_target/x86_64-unknown-linux-musl/debug/pseudo_mm_template_creator \
    --snapshot-path <snapshot_file> \
    --mem-file-path <memory_file> \
    --rdma-server <host:port> \
    --rdma-pgoff <page_offset> \
    --output-path <template_json> \
    [--hva-base <hex_hva>]
  ```
  - `snapshot_file` 与 `memory_file` 为 Firecracker checkpoint 生成的快照文件与内存文件。
  - `rdma-server` 指向能够写入内存镜像的 RDMA 服务端（例如 `10.10.1.2:19877`）。
  - `rdma-pgoff` 为上传时的页偏移，单位为页，如果省略则默认 `0`；多个模板需要自行避免重叠。
  - `hva-base` 可选，用于强制指定 pseudo_mm 映射到宿主的基地址（十六进制）。

- 批量生成模板（推荐在需要管理多份 checkpoint 时使用）：
  ```bash
  ./build/cargo_target/x86_64-unknown-linux-musl/debug/pseudo_mm_template_creator \
    --batch-config <batch_config.json>
  ```
  - `batch_config.json` 描述多份模板的输入输出信息，以及可选的全局默认值。示例：
    ```json
    {
      "rdma_server": "10.10.1.2:19877",
      "default_rdma_pgoff": 0,
      "hva_base": "0x700000000000",
      "templates": [
        {
          "snapshot_path": "tests/tmp/pseudomm-demo/vm.snapshot",
          "mem_file_path": "tests/tmp/pseudomm-demo/vm.mem",
          "output_path": "tests/tmp/pseudomm-demo/pseudo_mm_template_batch1.json"
        },
        {
          "snapshot_path": "tests/tmp/pseudomm-demo/vm.snapshot",
          "mem_file_path": "tests/tmp/pseudomm-demo/vm.mem",
          "output_path": "tests/tmp/pseudomm-demo/pseudo_mm_template_batch2.json"
        }
      ]
    }
    ```
  - 工具会自动为未指定的 `rdma_pgoff` 顺延上一份模板的页数，方便批量管理。

### 输入与输出

- **输入**：
  - Firecracker 快照文件与内存文件（必须与目标 VM 架构匹配）。
  - RDMA 服务端地址（TCP host:port），需保证可写入目标偏移。
  - 可选的 pseudo_mm 参数（HVA 基址、起始页偏移、批量配置）。

- **输出**：
  - 工具会在指定路径写出 `PseudoMmTemplate` JSON，包含：
    - `pseudo_mm_id`：在内核 pseudo_mm 模块中创建的实例编号，用于恢复端 `attach`。
    - `hva_base`：宿主侧虚拟地址基址（以字节计）。
    - `rdma_base_pgoff` 与 `rdma_image_size`：上传到 RDMA 的偏移与总字节数。
    - `regions`：每个 guest memory 区域的 GPA、HVA、大小与对应的 RDMA 偏移。
  - 同时，内存镜像会被流式写入到 RDMA 服务端提供的远端内存池。

### 配合恢复流程

1. **启动新的 Firecracker 进程**
   ```bash
   cd /root/faasnap-firecracker
   RUST_LOG=info ./build/cargo_target/x86_64-unknown-linux-musl/debug/firecracker \
     --api-sock /tmp/fc-pseudomm-restore.sock \
     --id pseudomm-restore
   ```
   - 该进程保持前台运行（可使用 `tmux`/后台方式）。
   - 等待 `/tmp/fc-pseudomm-restore.sock` 出现，表示 API 就绪。

2. **调用 `LoadSnapshot`**
   ```bash
   curl --unix-socket /tmp/fc-pseudomm-restore.sock \
     -H "Content-Type: application/json" \
     -X PUT http://localhost/snapshot/load \
     -d '{
       "snapshot_path": "/root/.../vm.snapshot",
       "mem_file_path": "/root/.../vm.mem",
       "enable_user_page_faults": false,
       "enable_diff_snapshots": false,
       "sock_file_path": "",
       "overlay_file_path": "",
       "overlay_regions": {},
       "ws_file_path": "",
       "ws_regions": [],
       "load_ws": false,
       "fadvise": "",
       "pseudo_mm_template_path": "/root/.../pseudo_mm_template.json"
     }'
   ```
   - `mem_file_path` 必须与模板生成时的文件一致（或根据后续改动传空字符串）。
   - `pseudo_mm_template_path` 指向由本工具输出的 JSON。

3. **恢复 VM 运行**
   ```bash
   curl --unix-socket /tmp/fc-pseudomm-restore.sock \
     -H "Content-Type: application/json" \
     -X PATCH http://localhost/vm \
     -d '{"state":"Resumed"}'
   ```
   - 进入 `Running` 状态后，pseudo_mm kernel 模块会把对应 `pseudo_mm_id` 绑定到当前 Firecracker 进程，VM 内存访问会触发 RDMA 缺页以按需拉取数据。

4. **可选校验**
   - 通过 `curl` 查询 Firecracker 日志或在宿主读取 `/var/log/kern.log`/`dmesg`，确认出现 `pseudo_mm RDMA fault` 的打印。
   - 如果 guest 提前配置了业务日志，可挂载 rootfs 查看最新输出，验证恢复后继续运行。

- 整个流程中模板文件无需修改，多个模板可分别指向不同的 `pseudo_mm_id` 与 `rdma_pgoff`，实现多 checkpoint 并行恢复。

