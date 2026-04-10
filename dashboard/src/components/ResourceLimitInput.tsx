import { useState, useRef, useCallback } from 'react';

interface ResourceLimitInputProps {
  label: string;
  value: number;
  onChange: (value: number) => void;
  unit: string;
  min: number;
  normalMax: number;
  absoluteMax: number;
  normalStep: number;
  overStep: number;
  highLabel: string;
  minLabel: string;
  normalMaxLabel: string;
  inputClass?: string;
}

export default function ResourceLimitInput({
  label,
  value,
  onChange,
  unit,
  min,
  normalMax,
  absoluteMax,
  normalStep,
  overStep,
  highLabel,
  minLabel,
  normalMaxLabel,
  inputClass = '',
}: ResourceLimitInputProps) {
  const [rawInput, setRawInput] = useState<string | undefined>(undefined);
  const [error, setError] = useState(false);
  const [displayValue, setDisplayValue] = useState(value);
  const sliderRef = useRef<HTMLInputElement>(null);
  const animRef = useRef<number>(0);

  const over = displayValue > normalMax;
  const sliderMin = over ? normalMax : min;
  const sliderMax = over ? absoluteMax : normalMax;
  const sliderStep = over ? overStep : normalStep;

  const animateTo = useCallback((target: number) => {
    cancelAnimationFrame(animRef.current);
    const start = displayValue;
    const diff = target - start;
    if (Math.abs(diff) < 0.01) { setDisplayValue(target); return; }
    const duration = 600;
    const startTime = performance.now();
    const step = (now: number) => {
      const elapsed = now - startTime;
      const t = Math.min(elapsed / duration, 1);
      // ease-out cubic
      const eased = 1 - Math.pow(1 - t, 3);
      setDisplayValue(start + diff * eased);
      if (t < 1) animRef.current = requestAnimationFrame(step);
    };
    animRef.current = requestAnimationFrame(step);
  }, [displayValue]);

  // Sync displayValue when value changes externally (e.g. slider drag)
  const prevValueRef = useRef(value);
  if (prevValueRef.current !== value) {
    setDisplayValue(value);
    prevValueRef.current = value;
  }

  return (
    <div>
      <div className="flex items-center justify-between mb-1.5">
        <span className="text-xs text-slate-400">{label}</span>
        <div className="flex items-center gap-1">
          <input
            type="text"
            inputMode="numeric"
            value={rawInput !== undefined ? rawInput : String(value)}
            onChange={e => {
              setRawInput(e.target.value);
              setError(false);
            }}
            onFocus={e => e.target.select()}
            onBlur={() => {
              if (rawInput === undefined) return;
              if (rawInput === '') {
                // User cleared the field — revert, don't show error
                setRawInput(undefined);
                setError(false);
                return;
              }
              const v = Number(rawInput);
              if (isNaN(v) || v < min) {
                setError(true);
                setRawInput(undefined);
              } else {
                onChange(v);
                setRawInput(undefined);
                setError(false);
                animateTo(v);
              }
            }}
            className={`w-20 border rounded px-2 py-0.5 text-xs text-right font-mono focus:outline-none focus:ring-1 transition-colors ${inputClass} ${
              error
                ? 'border-red-500/50 text-red-400 focus:border-red-500/50 focus:ring-red-500/25'
                : 'border-slate-700/50 text-violet-300 focus:border-violet-500/50 focus:ring-violet-500/25'
            }`}
          />
          <span className="text-xs text-slate-500">{unit}</span>
        </div>
      </div>
      <input
        ref={sliderRef}
        type="range"
        min={sliderMin}
        max={sliderMax}
        step={sliderStep}
        value={over ? Math.min(displayValue, absoluteMax) : displayValue}
        onChange={e => {
          const v = Number(e.target.value);
          setDisplayValue(v);
          onChange(v);
          setError(false);
        }}
        className={`w-full ${over ? 'accent-red-500' : 'accent-violet-500'}`}
      />
      <div className="flex justify-between text-[10px] text-slate-600 mt-0.5">
        <span>{over ? normalMaxLabel : minLabel}</span>
        {over && <span className="text-red-400/60">{highLabel}</span>}
        <span>{over ? `${absoluteMax} ${unit}` : normalMaxLabel}</span>
      </div>
    </div>
  );
}
