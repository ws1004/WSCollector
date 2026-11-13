import { filePrefix, isFat32, isExternal, isLocked, isOutputPart, kindLabel, loadDisks, partName, selectedTargets, toggleSelect, useApp } from "../store";
import { size } from "../format";
import { Icon } from "../components/Icon";
import { Seg } from "../components/Seg";
import type { Compression, DiskInfo, HashAlg, ImageFormat } from "../types";

const ROLE_TEXT: Record<string, string> = { tool: "수집 도구", output: "저장 위치" };

function DiskCard({ d }: { d: DiskInfo }) {
  const sel = useApp((s) => s.selection);
  const on = sel.includes(`d:${d.number}`);
  const locked = isLocked(d);
  const st = useApp();
  const role = d.roles.includes("output") ? "output" : d.roles.includes("tool") ? "tool" : null;
  const total = Number(d.size) || 1;
  return (
    <article className={`disk${on ? " is-on" : ""}${locked ? " is-locked" : ""}`}>
      <label className="disk__head">
        <input
          type="checkbox"
          checked={on}
          disabled={locked}
          aria-label={`Disk ${d.number} ${d.model} 전체${locked ? ", 저장 폴더가 있는 디스크라서 전체는 선택할 수 없음" : ""}`}
          onChange={(e) => toggleSelect(`d:${d.number}`, e.target.checked)}
        />
        <span className="box" aria-hidden="true"><Icon name="check" className="" /></span>
        <span className="disk__icon"><Icon name={isExternal(d) ? "usb" : "disk"} className="" /></span>
        <span className="disk__title"><b>{st.env?.os === "linux" ? d.path : `Disk ${d.number}`}</b><span>{d.model}</span></span>
        <span className="disk__tags">
          {d.roles.includes("output") && <span className="tag tag--role"><Icon name="lock" />{ROLE_TEXT.output}</span>}
          {d.roles.includes("tool") && <span className="tag tag--role">{ROLE_TEXT.tool}</span>}
          {d.roles.includes("system") && <span className="tag">시스템</span>}
          <span className="tag">{d.bus}</span>
          <span className="tag">{kindLabel(d)}</span>
        </span>
        <span className="disk__size">{size(d.size)}</span>
      </label>
      <div className="disk__meta mono" translate="no">
        {d.path}<span>S/N {d.serial || "알 수 없음"}</span><span>{d.partitionStyle}</span>
      </div>
      <div className="map" aria-hidden="true">
        {d.partitions.map((p, i) => (
          <span key={p.number} className={`map__seg map__seg--${i % 4}`} style={{ flexGrow: Math.max(Number(p.size) / total, 0.035) }} />
        ))}
      </div>
      {isExternal(d) && !locked && (
        <p className="disk__warn"><Icon name="alert" />외장으로 연결된 디스크입니다. 원본 증거라면 쓰기 방지 장치를 거쳐 연결하세요.</p>
      )}
      {role && (
        <p className="disk__note">
          {locked
            ? "증거 파일을 저장하는 디스크라서 디스크 전체는 고를 수 없습니다. 저장 폴더가 없는 다른 파티션은 고를 수 있습니다."
            : "WS Collector가 실행 중인 매체입니다. 이미징 대상으로도, 저장 위치로도 쓸 수 있습니다."}
        </p>
      )}
      <div className="parts">
        {d.partitions.map((p, i) => {
          const key = `p:${d.number}:${p.number}`;
          const pon = on || sel.includes(key);
          const used = p.free != null ? 1 - Number(p.free) / Math.max(Number(p.size), 1) : null;
          const outPart = isOutputPart(p, st);
          return (
            <label key={key} className={`part${pon ? " is-on" : ""}${on ? " is-inc" : ""}${outPart ? " is-locked" : ""}`} title={outPart ? "저장 폴더가 있는 파티션이라 고를 수 없습니다" : undefined}>
              <input
                type="checkbox"
                checked={pon}
                disabled={on || outPart}
                aria-label={`${partName(p)}${on ? ", 디스크 전체에 포함" : ""}${outPart ? ", 저장 폴더가 있어 선택할 수 없음" : ""}`}
                onChange={(e) => toggleSelect(key, e.target.checked)}
              />
              <span className="box" aria-hidden="true"><Icon name="check" className="" /></span>
              <span className={`part__swatch map__seg--${i % 4}`} aria-hidden="true" />
              <span className="part__name">
                {p.letter ? <><b>{p.letter}</b>{p.label ? ` ${p.label}` : ""}</> : <b>{p.kind || `파티션 ${p.number}`}</b>}
              </span>
              <span className="part__fs">{p.fs || (p.letter ? "" : `P${p.number}`)}</span>
              <span className="part__use" aria-hidden="true">{used != null && <i style={{ width: `${Math.round(used * 100)}%` }} />}</span>
              <span className="part__size">{size(p.size)}</span>
            </label>
          );
        })}
      </div>
    </article>
  );
}

function DiskList() {
  const st = useApp((s) => s.disksState);
  const disks = useApp((s) => s.disks);
  const err = useApp((s) => s.disksError);
  if (st === "loading")
    return (
      <div className="disks" aria-busy="true">
        <p className="visually-hidden">디스크 목록을 불러오는 중입니다…</p>
        {[0, 1, 2].map((i) => (
          <div key={i} className="skeleton" aria-hidden="true"><i /><i /><i /></div>
        ))}
      </div>
    );
  if (st === "error")
    return (
      <div className="state state--error">
        <Icon name="lock" className="state__ic" />
        <h3>디스크를 읽지 못했습니다</h3>
        <p>{err}</p>
        <button type="button" className="btn btn--primary" onClick={() => void loadDisks()}><Icon name="refresh" />다시 찾기</button>
      </div>
    );
  if (disks.length === 0)
    return (
      <div className="state">
        <Icon name="disk" className="state__ic" />
        <h3>이미징할 디스크가 없습니다</h3>
        <p>원본 디스크를 쓰기 방지 장치에 연결한 뒤 다시 찾아보세요.</p>
        <button type="button" className="btn btn--ghost" onClick={() => void loadDisks()}><Icon name="refresh" />디스크 다시 찾기</button>
      </div>
    );
  return (
    <div className="disks">
      {disks.map((d) => <DiskCard key={d.number} d={d} />)}
    </div>
  );
}

function DiskOptions() {
  const s = useApp();
  const ts = selectedTargets(s);
  const prefix = filePrefix(s);
  const e01 = s.format === "e01";
  const fat = isFat32(s);
  const ext = (n: number) => (e01 ? `.E${String(n).padStart(2, "0")}` : s.splitGiB ? `.${String(n).padStart(3, "0")}` : ".dd");
  const setFormat = (v: ImageFormat) => {
    // RAW는 FAT32면 2GB 분할로 고정, E01은 FAT32면 엔진이 알아서 나눔(4GB 조각은 불가)
    if (v === "raw" && fat) useApp.setState({ format: v, splitGiB: 2 });
    else if (v === "e01" && fat && s.splitGiB === 4) useApp.setState({ format: v, splitGiB: 0 });
    else useApp.setState({ format: v });
  };
  return (
    <div className="opts">
      <div className="opt">
        <span className="opt__label" id="lbl-fmt">이미지 형식</span>
        <Seg<ImageFormat> name="fmt" value={s.format} options={[{ v: "e01", label: "E01 (압축)" }, { v: "raw", label: "RAW (.dd)" }]} onChange={setFormat} />
      </div>
      {e01 && (
        <div className="opt">
          <span className="opt__label">압축</span>
          <Seg<Compression>
            name="compress"
            value={s.compression}
            options={[{ v: "best", label: "최대" }, { v: "fast", label: "빠르게" }, { v: "none", label: "안 함" }]}
            onChange={(v) => useApp.setState({ compression: v })}
          />
          {s.compression === "best" && <p className="field__help">파일이 가장 작아집니다. CPU가 약한 시스템에서는 디스크 읽기보다 느릴 수 있습니다.</p>}
        </div>
      )}
      <div className="opt">
        <span className="opt__label">{e01 ? "파일 나누기" : "파일 분할"}</span>
        <Seg<0 | 2 | 4>
          name="split"
          value={s.splitGiB}
          options={[{ v: 0, label: "나누지 않음", soon: fat && !e01 }, { v: 2, label: "2 GB" }, { v: 4, label: "4 GB", soon: fat }]}
          onChange={(v) => useApp.setState({ splitGiB: v })}
        />
        {fat && (
          <p className="field__help">
            {e01
              ? "저장 매체가 FAT32라서 4GB 넘는 파일을 만들 수 없습니다. '나누지 않음'이어도 약 3.9GB마다 .E02, .E03…으로 나눠 저장합니다."
              : "저장 매체가 FAT32라서 4GB 넘는 파일을 만들 수 없습니다. 2GB로 나눠 저장합니다."}
          </p>
        )}
      </div>
      <div className="opt">
        <span className="opt__label">해시</span>
        <div className="chips">
          {(["md5", "sha1", "sha256"] as HashAlg[]).map((a) => (
            <label key={a}>
              <input type="checkbox" checked={s.algs[a]} onChange={(e) => useApp.setState({ algs: { ...s.algs, [a]: e.target.checked } })} />
              <span>{{ md5: "MD5", sha1: "SHA-1", sha256: "SHA-256" }[a]}</span>
            </label>
          ))}
        </div>
      </div>
      <div className="opt opt--toggles">
        <label className="toggle">
          <span><b>이미징 후 검증</b><small>저장한 파일을 캐시 없이 다시 읽어 해시를 비교합니다</small></span>
          <input type="checkbox" checked={s.verify} onChange={(e) => useApp.setState({ verify: e.target.checked })} />
          <span className="switch" aria-hidden="true" />
        </label>
        <label className="toggle">
          <span><b>읽기 오류 구간 0으로 채우기</b><small>끄면 오류가 난 지점에서 이미징을 멈춥니다</small></span>
          <input type="checkbox" checked={s.zeroFill} onChange={(e) => useApp.setState({ zeroFill: e.target.checked })} />
          <span className="switch" aria-hidden="true" />
        </label>
      </div>
      <div className="opt">
        <span className="opt__label">만들어질 파일</span>
        <ul className="files">
          {ts.length === 0 && <li className="empty">왼쪽에서 디스크나 볼륨을 고르세요.</li>}
          {ts.map((t) => {
            // E01은 압축 후 크기를 미리 알 수 없어 조각 수를 셀 수 없음 (원본 크기 기준 최대치)
            const unit = s.splitGiB ? s.splitGiB * 1024 ** 3 : e01 && fat ? 4000 * 1024 ** 2 : 0;
            const parts = unit ? Math.max(1, Math.ceil(t.size / unit)) : 1;
            const name = `${prefix}_${t.tag}${ext(1)}`;
            return (
              <li key={t.key}>
                <Icon name="file" />
                <span className="mono" title={name}>{name}{parts > 1 ? (e01 ? " …" : ` … ${ext(parts)}`) : ""}</span>
                <span>{e01 ? `원본 ${size(t.size)}` : size(t.size)}</span>
              </li>
            );
          })}
        </ul>
      </div>
    </div>
  );
}

function MemoryOptions() {
  const env = useApp((s) => s.env);
  const prefix = filePrefix(useApp());
  const m = env?.memory;
  if (!m) {
    return (
      <div className="opts">
        <div className="infobar infobar--info">
          <Icon name="info" />
          <p>
            {env?.os === "linux"
              ? `이 시스템에서는 메모리를 수집할 수 없습니다. ${env?.memoryError ?? "root 권한으로 실행했는지, 커널 lockdown이 켜져 있는지 확인하세요."}`
              : `이 PC에서는 메모리를 수집할 수 없습니다. ${env?.memoryError ?? "관리자 권한으로 실행했는지, 도구 폴더의 drivers 폴더에 winpmem_x64.sys가 있는지 확인하세요."}`}
          </p>
        </div>
        <dl className="props">
          <div><dt>물리 메모리</dt><dd>{size(env?.host.ramBytes ?? 0)}</dd></div>
        </dl>
      </div>
    );
  }
  return (
    <div className="opts">
      <dl className="props">
        <div><dt>수집할 메모리</dt><dd>{size(m.bytes)} (영역 {m.ranges}개)</dd></div>
        <div><dt>원본</dt><dd className="mono">{m.source}</dd></div>
        <div><dt>형식</dt><dd>LiME (Volatility 등에서 바로 분석)</dd></div>
        <div><dt>실행 순서</dt><dd>디스크 이미징보다 먼저 (메모리 내용이 계속 바뀌기 때문)</dd></div>
        {m.driver ? (
          <div><dt>드라이버</dt><dd>WinPmem을 도구 매체에서 적재하고, 메모리 수집이 끝나면 바로 서비스를 삭제합니다 (보고서 흔적 기록에 남음)</dd></div>
        ) : (
          <div><dt>커널 모듈</dt><dd>적재하지 않음 (AVML과 같은 방식)</dd></div>
        )}
      </dl>
      <div className="opt">
        <span className="opt__label">만들어질 파일</span>
        <div className="dest">
          <Icon name="file" />
          <span>
            <b className="mono">{prefix}_Memory.lime</b>
            <small>{size(m.imageBytes)}. 저장 매체가 FAT32면 나눠 저장합니다</small>
          </span>
        </div>
      </div>
      {m.notes.length > 0 && <p className="field__help">원본 확인: {m.notes.join(", ")}</p>}
    </div>
  );
}

export function SelectScreen() {
  const env = useApp((s) => s.env);
  const taskDisk = useApp((s) => s.taskDisk);
  const taskMem = useApp((s) => s.taskMem);
  const optTab = useApp((s) => s.optTab);
  const disksCount = useApp((s) => s.disks.length);
  const disksState = useApp((s) => s.disksState);
  const ts = selectedTargets(useApp());
  const h = env?.host;
  return (
    <section className="screen" aria-labelledby="h-select">
      <header className="host">
        <div className="host__icon"><Icon name="pc" className="" /></div>
        <div className="host__body">
          <h1 id="h-select" tabIndex={-1}>{h?.hostname ?? "대상 시스템"}</h1>
          {h && (
            <dl className="host__specs">
              <div><dt>운영체제</dt><dd>{h.osName} {h.osVersion}</dd></div>
              <div><dt>{env?.os === "linux" ? "커널" : "빌드"}</dt><dd>{h.osBuild}</dd></div>
              <div><dt>프로세서</dt><dd>{h.cpu}</dd></div>
              <div><dt>메모리</dt><dd>{size(h.ramBytes)}</dd></div>
              <div><dt>부팅</dt><dd>{h.bootTime}</dd></div>
            </dl>
          )}
        </div>
      </header>

      <div className="tasks">
        <label className={`task${taskDisk ? " is-on" : ""}`}>
          <input type="checkbox" checked={taskDisk} onChange={(e) => useApp.setState({ taskDisk: e.target.checked })} />
          <span className="task__icon"><Icon name="disk" className="" /></span>
          <span className="task__text"><b>디스크 이미징</b><small>{taskDisk ? `대상 ${ts.length}개, ${size(ts.reduce((a, t) => a + t.size, 0))}` : "이번 수집에서 제외"}</small></span>
          <span className="switch" aria-hidden="true" />
        </label>
        {env?.memory ? (
          <label className={`task${taskMem ? " is-on" : ""}`}>
            <input type="checkbox" checked={taskMem} onChange={(e) => useApp.setState({ taskMem: e.target.checked, optTab: e.target.checked ? "mem" : optTab })} />
            <span className="task__icon"><Icon name="mem" className="" /></span>
            <span className="task__text">
              <b>메모리 덤프</b>
              <small>{taskMem ? `${size(env.memory.bytes)}, ${env.memory.source}, 디스크보다 먼저` : "이번 수집에서 제외"}</small>
            </span>
            <span className="switch" aria-hidden="true" />
          </label>
        ) : (
          <label className="task is-disabled" aria-disabled="true">
            <input type="checkbox" checked={false} disabled readOnly />
            <span className="task__icon"><Icon name="mem" className="" /></span>
            <span className="task__text"><b>메모리 덤프</b><small>{env?.os === "linux" ? "이 시스템에서 읽을 수 있는 원본 없음" : "수집 불가 (메모리 탭에서 이유 확인)"}</small></span>
            <span className="switch" aria-hidden="true" />
          </label>
        )}
      </div>

      <div className="grid">
        <section className="panel panel--disks" aria-labelledby="h-disks">
          <header className="panel__head">
            <h2 id="h-disks">디스크와 볼륨</h2>
            <span className="count">{disksState === "ready" ? `${disksCount}개 발견` : disksState === "loading" ? "찾는 중…" : ""}</span>
            <button type="button" className="icon-btn" aria-label="디스크 목록 새로고침" disabled={disksState === "loading"} onClick={() => void loadDisks()}>
              <Icon name="refresh" className="" />
            </button>
          </header>
          <DiskList />
        </section>

        <section className="panel panel--opts" aria-labelledby="h-opts">
          <header className="panel__head panel__head--tabs">
            <h2 id="h-opts" className="visually-hidden">수집 옵션</h2>
            <div className="selector" role="group" aria-label="옵션 종류">
              <button type="button" className="selector__item" aria-pressed={optTab === "disk"} onClick={() => useApp.setState({ optTab: "disk" })}>이미징 옵션</button>
              <button type="button" className="selector__item" aria-pressed={optTab === "mem"} onClick={() => useApp.setState({ optTab: "mem" })}>메모리 옵션</button>
            </div>
          </header>
          {optTab === "disk" ? <DiskOptions /> : <MemoryOptions />}
        </section>
      </div>
    </section>
  );
}
