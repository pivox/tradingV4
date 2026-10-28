<?php

declare(strict_types=1);

namespace App\Tests\Report;

use App\Report\Format;
use PHPUnit\Framework\TestCase;

final class FormatTest extends TestCase
{
    public function testMoneyUsesFrenchSeparators(): void
    {
        $format = new Format();

        self::assertSame("1\u{202F}234,57", $format->money(1234.567));
        self::assertSame("\u{2212}5,64", $format->money(-5.644));
        self::assertSame('0,00', $format->money(-0.0001));
        self::assertSame('—', $format->money(null));
    }

    public function testPercent(): void
    {
        self::assertSame("1,23\u{00A0}%", (new Format())->percent(0.0123));
        self::assertSame("\u{2212}6,78\u{00A0}%", (new Format())->percent(-0.0678));
    }

    public function testDateTimeUtc(): void
    {
        self::assertSame('2021-01-01 04:10 UTC', (new Format())->dateTime(1_609_474_200_000));
        self::assertSame('—', (new Format())->dateTime(null));
    }

    public function testDuration(): void
    {
        $format = new Format();

        self::assertSame('45 s', $format->duration(45_000.0));
        self::assertSame('12 min', $format->duration(720_000.0));
        self::assertSame('5 h 30 min', $format->duration(19_800_000.0));
        self::assertSame('2 h', $format->duration(7_200_000.0));
        self::assertSame('1 j 3 h', $format->duration(97_200_000.0));
    }

    public function testScalar(): void
    {
        $format = new Format();

        self::assertSame('true', $format->scalar(true));
        self::assertSame('0.005', $format->scalar(0.005));
        self::assertSame('0.00000001', $format->scalar(1e-8));
        self::assertSame('2', $format->scalar(2.0));
        self::assertSame('null', $format->scalar(null));
    }
}
