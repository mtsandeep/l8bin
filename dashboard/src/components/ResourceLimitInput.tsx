import { Minus, Plus } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';

interface ResourceLimitInputProps {
  label: string;
  value: number; // 0 = unlimited
  onChange: (value: number) => void;
  unit: string;
  min: number;
  step: number;
  defaultValue: number;
  decimals?: number;
  inputClass?: string;
}

function roundTo(n: number, decimals: number): number {
  const factor = 10 ** decimals;
  return Math.round(n * factor) / factor;
}

export default function ResourceLimitInput({
  label,
  value,
  onChange,
  unit,
  min,
  step,
  defaultValue,
  decimals = 0,
  inputClass = 'bg-slate-900/50',
}: ResourceLimitInputProps) {
  const [rawInput, setRawInput] = useState<string | undefined>(undefined);
  const [error, setError] = useState(false);
  const lastNonZeroRef = useRef<number>(value > 0 ? value : defaultValue);

  const isUnlimited = value === 0;

  useEffect(() => {
    if (value > 0) {
      lastNonZeroRef.current = value;
    }
  }, [value]);

  const commitRaw = () => {
    if (rawInput === undefined) return;
    if (rawInput.trim() === '') {
      setRawInput(undefined);
      setError(false);
      return;
    }
    const v = Number(rawInput);
    if (Number.isNaN(v) || v < min) {
      setError(true);
      setRawInput(undefined);
    } else {
      onChange(roundTo(v, decimals));
      setRawInput(undefined);
      setError(false);
    }
  };

  const handleDecrement = () => {
    if (isUnlimited) return;
    const prevBlock = (Math.ceil(value / step - 1e-9) - 1) * step;
    const next = roundTo(Math.max(min, prevBlock), decimals);
    setRawInput(undefined);
    setError(false);
    onChange(next);
  };

  const handleIncrement = () => {
    if (isUnlimited) return;
    const nextBlock = (Math.floor(value / step + 1e-9) + 1) * step;
    const next = roundTo(nextBlock, decimals);
    setRawInput(undefined);
    setError(false);
    onChange(next);
  };

  const toggleUnlimited = (checked: boolean) => {
    setRawInput(undefined);
    setError(false);
    if (checked) {
      if (value > 0) lastNonZeroRef.current = value;
      onChange(0);
    } else {
      onChange(lastNonZeroRef.current > 0 ? lastNonZeroRef.current : defaultValue);
    }
  };

  return (
    <div className="min-w-0">
      <div className="flex items-center justify-between gap-1.5 mb-1.5">
        <span className="text-xs text-slate-400 truncate">{label}</span>
        <label className="flex items-center gap-1 text-[10px] text-slate-400 hover:text-slate-200 cursor-pointer select-none shrink-0">
          <input
            type="checkbox"
            checked={isUnlimited}
            onChange={(e) => toggleUnlimited(e.target.checked)}
            className="rounded border-slate-600 bg-slate-800 text-violet-500 focus:ring-violet-500/30 focus:ring-offset-0 cursor-pointer"
          />
          <span>Max</span>
        </label>
      </div>

      <div className="flex items-center gap-1">
        <button
          type="button"
          onClick={handleDecrement}
          disabled={isUnlimited || value <= min}
          aria-label={`Decrease ${label}`}
          className="p-1 rounded border border-slate-700/60 bg-slate-800/70 text-slate-300 hover:bg-slate-700 hover:text-white disabled:opacity-35 disabled:cursor-not-allowed transition-colors cursor-pointer shrink-0"
        >
          <Minus size={11} />
        </button>

        <div
          className={`flex-1 min-w-0 flex items-center border rounded px-1.5 py-1 transition-colors focus-within:ring-1 ${inputClass} ${
            isUnlimited
              ? 'border-slate-700/40 cursor-not-allowed'
              : error
                ? 'border-red-500/50 focus-within:border-red-500/50 focus-within:ring-red-500/25'
                : 'border-slate-700/50 focus-within:border-violet-500/50 focus-within:ring-violet-500/25'
          }`}
        >
          <input
            type="text"
            inputMode="decimal"
            disabled={isUnlimited}
            value={isUnlimited ? 'Unlimited' : rawInput !== undefined ? rawInput : String(value)}
            onChange={(e) => {
              setRawInput(e.target.value);
              setError(false);
            }}
            onFocus={(e) => {
              if (!isUnlimited) e.target.select();
            }}
            onBlur={commitRaw}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                commitRaw();
              }
            }}
            className={`w-full min-w-0 bg-transparent text-xs font-mono text-center focus:outline-none ${
              isUnlimited ? 'text-slate-500 cursor-not-allowed' : error ? 'text-red-400' : 'text-violet-300'
            }`}
          />
          {!isUnlimited && <span className="text-[10px] text-slate-500 shrink-0 ml-1 select-none">{unit}</span>}
        </div>

        <button
          type="button"
          onClick={handleIncrement}
          disabled={isUnlimited}
          aria-label={`Increase ${label}`}
          className="p-1 rounded border border-slate-700/60 bg-slate-800/70 text-slate-300 hover:bg-slate-700 hover:text-white disabled:opacity-35 disabled:cursor-not-allowed transition-colors cursor-pointer shrink-0"
        >
          <Plus size={11} />
        </button>
      </div>
    </div>
  );
}
