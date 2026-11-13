// 분할 선택 (라디오 그룹)
export function Seg<T extends string | number>(props: { name: string; value: T; label?: string; options: { v: T; label: string; soon?: boolean | string }[]; onChange: (v: T) => void }) {
  return (
    <div className="seg" role="radiogroup" aria-label={props.label}>
      {props.options.map((o) => (
        <label key={String(o.v)} className={o.soon ? "is-soon" : undefined}>
          <input type="radio" name={props.name} checked={props.value === o.v} disabled={!!o.soon} onChange={() => props.onChange(o.v)} />
          <span>{o.label}{o.soon === "next" && <em>다음 버전</em>}</span>
        </label>
      ))}
    </div>
  );
}
