<?php

declare(strict_types=1);

namespace App\Tests\Report;

use App\Report\Chart\DrawdownChart;
use App\Report\Chart\HistogramChart;
use App\Report\Chart\LineChart;
use App\Report\Chart\ScatterChart;
use App\Report\Format;
use App\Report\ReportExtension;
use App\Report\ReportRenderer;
use App\Run\RunDirectoryReader;
use App\Tests\Support\RunFixture;
use RuntimeException;
use Symfony\Bundle\FrameworkBundle\Test\KernelTestCase;
use Twig\Environment;
use Twig\Loader\FilesystemLoader;

/**
 * Un rapport ne doit jamais refléter un template antérieur : le cache Twig compilé est désactivé, donc
 * chaque lancement de `bin/console` (nouveau processus, nouvel Environment) recompile les templates.
 */
final class TemplateFreshnessTest extends KernelTestCase
{
    private ?string $templates = null;

    protected function tearDown(): void
    {
        if (null !== $this->templates) {
            foreach (array_reverse(self::entries($this->templates)) as $entry) {
                $entry->isDir() ? rmdir($entry->getPathname()) : unlink($entry->getPathname());
            }
            rmdir($this->templates);
        }
        parent::tearDown();
    }

    /**
     * Contenu d'un dossier, parents avant enfants.
     *
     * @return list<\SplFileInfo>
     */
    private static function entries(string $directory): array
    {
        $entries = [];
        $iterator = new \RecursiveIteratorIterator(
            new \RecursiveDirectoryIterator($directory, \FilesystemIterator::SKIP_DOTS),
            \RecursiveIteratorIterator::SELF_FIRST,
        );
        foreach ($iterator as $entry) {
            if ($entry instanceof \SplFileInfo) {
                $entries[] = $entry;
            }
        }

        return $entries;
    }

    private function copyTemplates(string $source): string
    {
        $target = sys_get_temp_dir() . '/tv4-templates-' . bin2hex(random_bytes(6));
        $this->templates = $target;
        mkdir($target, 0o755, true);
        foreach (self::entries($source) as $entry) {
            $destination = $target . substr($entry->getPathname(), strlen($source));
            $entry->isDir() ? mkdir($destination, 0o755, true) : copy($entry->getPathname(), $destination);
        }

        return $target;
    }

    public function testTwigCompiledCacheIsDisabled(): void
    {
        $twig = self::getContainer()->get('twig');
        self::assertInstanceOf(Environment::class, $twig);

        self::assertFalse($twig->getCache(true));
    }

    public function testModifiedTemplateIsReflectedByTheNextRender(): void
    {
        $twig = self::getContainer()->get('twig');
        self::assertInstanceOf(Environment::class, $twig);
        $loader = $twig->getLoader();
        self::assertInstanceOf(FilesystemLoader::class, $loader);
        $reader = self::getContainer()->get(RunDirectoryReader::class);
        self::assertInstanceOf(RunDirectoryReader::class, $reader);
        $run = $reader->read(RunFixture::SOURCE);

        $first = $this->renderer($twig)->render($run);
        self::assertStringContainsString('rapport tradingV4</title>', $first);

        // Nouveau lancement : copie modifiée des templates et Environment neuf, comme un nouveau processus.
        $copy = $this->copyTemplates(dirname(__DIR__, 2) . '/templates');
        $file = $copy . '/report/standalone.html.twig';
        $content = file_get_contents($file);
        if (false === $content) {
            throw new RuntimeException('template illisible');
        }
        file_put_contents($file, str_replace('rapport tradingV4</title>', 'rapport MODIFIE</title>', $content));
        $loader->setPaths([$copy]);
        $fresh = new Environment($loader, ['cache' => false, 'strict_variables' => true]);
        $fresh->addExtension(new ReportExtension(new Format()));

        $second = $this->renderer($fresh)->render($run);

        self::assertStringContainsString('rapport MODIFIE</title>', $second);
        self::assertStringNotContainsString('rapport tradingV4</title>', $second);
    }

    private function renderer(Environment $environment): ReportRenderer
    {
        $format = new Format();

        return new ReportRenderer(
            $environment,
            $format,
            new LineChart($format),
            new DrawdownChart(new LineChart($format), $format),
            new ScatterChart($format),
            new HistogramChart($format),
        );
    }
}
