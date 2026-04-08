import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "SSR Load Test Store",
  description: "Next.js SSR load testing application",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
