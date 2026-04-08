import { NextResponse } from "next/server";

export async function GET() {
  return NextResponse.json({
    status: "ok",
    uptime: process.uptime(),
    memory: process.memoryUsage(),
    timestamp: new Date().toISOString(),
  });
}
