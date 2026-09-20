# spectra-lab

本地运行的光谱基线校正与峰拟合工具：纯 Rust 后端 + 原生 canvas 交互页面 +
SQLite 持久化。支持在同一条原始曲线上对比多种基线区间、平滑尺度与
Gauss/Lorentz/Voigt 重叠峰模型，所有人工选择、拟合参数和原始数据通过
SHA-256 哈希串成可复查记录。

## 安装与运行

```bash
cargo fetch --locked
cargo test --locked
cargo run --locked -- --listen 127.0.0.1:5332
# 打开 http://127.0.0.1:5332
```

仅依赖四个 crate（`rusqlite/bundled`、`serde`、`serde_json`、`sha2`）；
HTTP 服务、HTML 解析、线性代数、非线性拟合均为仓库内实现。

首次点击 **“载入固定种子合成曲线”** 即可演示：380–720 nm、3 个重叠峰
（Gauss/Gauss/Lorentz）、二次基线、高斯噪声与 3 个注入的宇宙射线尖峰；
生成参数（真值）随上传响应一并返回，便于核对拟合结果。

## 工作流与审计模型

- 原始 CSV **永远不被覆盖**。数据库 `datasets.raw_csv` 存原始文本，
  `input_hash = SHA-256(raw_csv)`；所有曲线/拟合都引用该哈希。
- 基线、平滑曲线存于 `artifacts` 表，每条记录携带 `input_hash`、
  `method_version`、参数 JSON、上游工件哈希与自身 `artifact_hash`。
  平滑曲线的上游是基线曲线（`upstream_hash`），形成哈希链。
- 方案（`schemes`）不可变：调整任何峰初值、锁定/解锁峰位、改变基线区间或
  排除点都产生**新行**，`parent_id` 指向上一版；旧参数、排除点和基线范围
  始终可重放。
- 拟合作业按 `SHA-256(input_hash · 规范化方案JSON · model/fitter/scheme 版本)`
  去重（`jobs.fingerprint` 唯一）。相同指纹直接重放，不重复计算。
- 作业结果在**单个 SQLite 事务**中发布（job + fit + artifacts），
  进程中断不会出现“半套参数”。启动时把残留的 `running` 作业复位为
  `queued` 并递增 `attempts`，可安全重试。
- 自动最优候选标记为 `origin=auto_candidate`，**在用户显式二次确认前
  不会取代任何人工已选方案**（后端对静默提升返回 409）。

## 横轴单位变换

支持 `nm`（波长）、`cm^-1`（波数，= 10⁷/nm）、`eV`（能量，= hc/λ，
`hc = 1239.8419843929196 eV·nm`，CODATA 2018）。

- 倒数型变换会**反转排序**；区间变换后端点自动重新排序。
- 区间用两个端点各自映射，因此转换后仍覆盖同一物理范围，并有测试
  `units::tests::interval_preserves_range_through_inversion` 校验。
- 峰宽采用“对称跨度约定”：把 `[c−w/2, c+w/2]` 两个端点显式映射，
  新宽度 = 变换后区间宽度。倒数变换天然不对称，变换后是以映射峰位为中心的
  对称近似；往返误差与非线性度同阶（测试容差 1e-5 相对宽度）。
- 页面的单位选择只影响显示；方案中的区间、排除点、峰位/宽度始终以
  **原始数据集单位**存储，前端 `app.js` 与后端 `src/units.rs` 使用相同常量。

## 输入诊断（彼此独立，互不掩盖）

| 诊断 | 行为 |
|---|---|
| 非有限数（NaN/±Inf） | 该行丢弃并记录 1 基行号 |
| 重复 x | 列出重复坐标，仍允许加载 |
| 非单调输入 | 报告逆序位置与方向（递减序列视为合法单调） |
| 样本太少 | 有效点 < 8 时拒绝拟合（`MIN_ROWS`） |
| 解析错误 | 记录行号与原因 |

## 峰模型与拟合

- **Gauss**：`h·exp(−4ln2·((x−c)/w)²)`，w 为 FWHM，面积 `h·w·√(π/(4ln2))`。
- **Lorentz**：`h/(1+4((x−c)/w)²)`，面积 `h·w·π/2`。
- **Voigt**：以 Gaussian 1-σ `sigma` 与 Lorentzian HWHM `gamma` 参数化，
  用 Faddeeva 函数 `w(z)` 的 64 节点 Gauss–Hermite 求积计算，面积是直接
  参数；`gamma=0` 时退化到 Gauss 闭式（避免实轴求积丢项）。
- 拟合器：Levenberg–Marquardt。中心为线性自由参数，宽度/高度/面积/Voigt
  尺度在 **log 空间**拟合，天然保证正性且步长尺度不变；`fix_center` 的峰位
  从自由向量中剔除。
- 协方差：`σ²·(JᵀJ)⁻¹`，`σ² = RSS/dof`；页面显示自由参数相关矩阵。
- **面积 95% 置信区间**：delta 法。对面积按自由参数中心差分得梯度 `g`，
  `SE = √(gᵀ·Cov·g)`，CI = 面积 ± 1.959964·SE（大样本正态临界值）；
  log 参数的链式法则已由差分自动包含。小样本下 delta 法为近似，CI 覆盖率
  测试（12 条固定种子噪声曲线）使用宽松下限以防 flaky。
- 指标：`RMSE`、`AIC = n·ln(RSS/n)+2k`、`BIC = n·ln(RSS/n)+k·ln n`。
- 平滑（Gauss 核加权，σ 由用户指定）仅用于显示；拟合始终使用未平滑的
  去基线数据。基线为用户区间上的加权多项式最小二乘（0–6 次，Cholesky 求解）。

## 不收敛状态

不收敛时（达到迭代上限、无有限步、欠定）拟合**仍被持久化**，
`fits.converged = 0`，响应 `report.status` 给出原因（`max_iterations`、
`no_finite_step`、`underdetermined`），并保留每次迭代的 χ²、阻尼因子、
是否被接受以及**最后有限参数向量**（`report.trace`）用于诊断。
此类结果在 UI 中以红色标注，**永远不会被标记为已接受方案**，也不参与
自动候选的静默提升。

## 数值容差与版本

| 量 | 值 |
|---|---|
| 中心差分步长 | `1e-7 · max(1, |q|)` |
| LM 初始阻尼 λ | `1e-3`，成功 ×0.3，失败 ×10 |
| 最大迭代 | 200 |
| χ² 相对变化收敛阈值 | `1e-10` |
| 参数步长收敛阈值 | `1e-10` |
| 放弃阈值 | λ > `1e12` |
| 特征值 QL 小次对角阈值 | `1e-15·(|d_m|+|d_{m+1}|)` |
| 排除点匹配 | 相对坐标 `1e-9` |
| 最少有效行 | 8 |
| 基线最高次数 | 6（锚点数需 ≥ 次数+2） |
| Voigt 数值下限 | `gamma ≥ sigma·1e-10`（再小走 Gauss 闭式） |
| 合成曲线 RNG | xorshift128+ + Box–Muller，固定种子 `20260921` |

方法版本字符串（进入指纹与工件记录）：

- `units-v1.0.0` / `csv-loader-v1.0.0` / `model-v1.0.0`
- `lma-v1.0.0`（拟合器）/ `scheme-v1.0.0`
- `rng-xorshift128plus-v1`，schema version = 1

## HTTP API

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/upload` | `{csv}` 或 `{synthetic:true}`，返回数据、诊断、版本 |
| POST | `/api/convert` | 单位标量/区间/峰（中心+宽度）变换 |
| POST | `/api/fit` | `{dataset_id, spec, auto_candidate?}`，同指纹重放 |
| POST | `/api/schemes/create` | 单独落一条人工方案 |
| GET | `/api/schemes?dataset_id=` | 方案历史 |
| POST | `/api/schemes/accept` | 确认/撤销；自动候选需 `confirm_promote_auto:true` |
| GET | `/api/datasets` | 已载入数据集 |

## 测试

```bash
cargo test --locked
```

- 21 个单元测试：单位往返/区间物理范围、CSV 独立诊断、Gauss/Lorentz/Voigt
  面积一致性与 Voigt 归一化、LM 重叠峰恢复、固定峰位、噪声 CI 覆盖率、
  基线/排除流水线、RNG 统计性质、指纹稳定性、合成曲线确定性。
- 4 个端到端 HTTP 测试（`tests/api_flow.rs`）：合成数据→拟合→去重重放→
  锁定峰位产生新指纹→多方案共存、自动候选 409 保护、单位换算、坏输入诊断、
  不收敛不被接受。

## 页面交互

- 工具栏切换：平移 / 鼠标圈定基线区间 / 点击屏蔽宇宙射线点 / 放置峰候选
  （选择 Gauss/Lorentz/Voigt、初宽与锁定峰位）。
- 主图叠加原始点、基线、去基线曲线、平滑曲线、拟合模型；下方面板显示残差。
- 右侧结果区显示 RMSE/AIC/BIC、峰参数、面积 95% CI 与自由参数相关热力矩阵。
- 底部方案表列出所有人工作品与自动候选，可显式确认；确认状态即审计结论。
