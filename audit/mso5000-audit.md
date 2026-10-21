# MSO5000 仪器包手册审计

- 手册：`MSO5000_ProgrammingGuide_CN.pdf`（提取文本共 506 个命令章节、553 个命令路径/形式）
- 仪器包：`rigol.mso5000`，审计前 895 条，修正后 **889 条**，唯一命令头 **889** 条
- 逐条结果：见 [mso5000-audit.csv](mso5000-audit.csv)

## 结论

| 检查项 | 结果 |
| --- | --- |
| 命令头在手册中存在 | **889/889** |
| 可写命令有 set 形式 | **828/828** |
| 可读命令有 query 形式 | **819/819** |
| 重复命令头 | **0** |
| 手册/实现参数数量冲突（扣除 *RCL 手册漏写） | **0** |
| 选件归属不匹配 | **0** |
| 手册自身漏写参数 | **1** (`*RCL`) |
| 可选参数未完全暴露 | **3** |

## 已修正

- `gen2.impedance`：set 形式从 `:SOURce2:OUTPut1:IMPedance` 改为 `:SOURce2:OUTPut2:IMPedance`，去掉与 set 不一致的 `query_header`。
- `la.autosort`：手册只有 `:LA:AUTOsort <n>` 写形式，删除 `readable` 和 `result`。
- 删除 6 条重复的 `gen_*` 别名：`gen_amp`、`gen_freq`、`gen_impedance`、`gen_offset`、`gen_phase`、`gen_type`；保留 `gen1.*`。
- `gen1.output` / `gen2.output` 补上 `requires: ["AWG"]`。
- 895 → 889 条，唯一命令头 889 条。

## 例外

- `*RCL`：手册只写 `*RCL`，但 SCPI 标准和 Python 库均使用寄存器号；保留 1 个参数，并在 summary 注明手册漏写。
- 3 条命令的可选参数没有全部暴露，因为当前 Rust 执行器要求参数个数固定：`trigger.pattern.pattern`（1/19）、`trigger.duration.type`（1/19）、`meas.measure`（2/3）。
- 审计时 861 个生成参数为 `any`；后续已为常用命令补充 typed schema（float/int/bool/enum、min/max），MSO 仍有 791 个 `any` 参数待按手册逐项建模。

## 选件分布

| 选件 | 命令数 |
| --- | ---: |
| AWG | 62 |
| EMBD | 82 |
| COMP | 48 |
| AUTO | 43 |
| AUDIO | 32 |
| FLEX | 20 |
| AERO | 9 |
| PWR | 8 |
| 无选件要求 | 585 |

## 实机验证

- 重新上电后 `scpi_load` 真实 MSO5104：**889 条命令，0 条隐藏**，`options.installed` 为 AWG/BND/COMP/EMBD/AUTO/FLEX/AUDIO/AERO/PWR，`options.known=true`，无错误。
- 只读 `ch1.scale` = 5；`meas.measure VPP,CHANnel2` = `9.9000E+37`，是 Rigol 的无有效测量哨兵；DG1022Z CH1 输出 OFF，CH2 无信号，符合预期。
- 未使用 `*OPT?`、未查询非法 type、未做任何写操作。

## 方法

1. 用 `pdftotext -layout` 提取手册，按列首 SCPI 标题和连续多命令组切分章节。
2. 展开 `[...]` 可选段，按 SCPI 长短形式归一化命令路径；逐条比对 profile 的 `header`。
3. 从 `命令格式` 块提取 set/query 形式、参数个数上下界；与 profile 的 `writable/readable/args` 比对。
4. 按手册 `（选件）` 标注和 MSO5000 选件枚举核对 `requires`；通道号按 id 前缀核对。
5. 逐条结果写入 CSV；修正项写回 profile 并重新打包。
